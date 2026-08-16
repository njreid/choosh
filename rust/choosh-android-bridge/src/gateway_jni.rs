//! JNI boundary for `ai.choosh.WebGatewayBridge` and
//! `ai.choosh.MarkdownGatewayBridge` — separate Kotlin objects from
//! `ai.choosh.NativeBridge`/`ai.choosh.TerminalBridge`, matching this
//! crate's existing "unrelated responsibilities get separate JNI classes"
//! discipline (see `terminal_jni.rs`'s module doc).
//!
//! Each gateway (`crate::web_gateway`/`crate::markdown_gateway`) runs on its
//! own dedicated [`Runtime`] — a light echo of `lib.rs`/`terminal_jni.rs`'s
//! existing "one `Runtime` per long-lived native session" shape — started
//! and stopped from the synchronous JNI call sites below via `block_on`.
//! Every `opener`/`fetcher` closure handed to a gateway calls
//! [`crate::connection_engine_arc`]'s `Arc<Engine>` directly (no
//! `block_on`, see that function's doc comment) so a gateway's own
//! per-connection async tasks never re-enter the connection's `Runtime`.

use crate::jni_support::{HandleRegistry, jstring_to_string};
use crate::markdown_gateway::{self, FetchError, FetchedFile, FileFetcher, MarkdownGatewayCaps};
use crate::web_gateway::{self, GatewayCaps, RealUpstream, Upstream, UpstreamOpener};
use jni::objects::{JClass, JString};
use jni::sys::{jint, jlong};
use jni::{Env, native_method};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use tokio::runtime::Runtime;

// --- WebGatewayBridge ----------------------------------------------------

struct WebGatewaySession {
    gateway: web_gateway::RunningGateway,
    runtime: Runtime,
}

static WEB_GATEWAYS: LazyLock<HandleRegistry<WebGatewaySession>> = LazyLock::new(HandleRegistry::new);

/// Starts a `WebService` loopback gateway for `item_id`, tunneling through
/// the live connection behind `connection_handle`. Returns `0` if not
/// connected, the bind fails, or the connection handle is unknown — every
/// case Kotlin treats identically (no gateway to show).
#[allow(clippy::needless_pass_by_value, clippy::unnecessary_wraps)]
fn native_web_gateway_start<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    connection_handle: jlong,
    target_device_id: JString<'local>,
    item_id: JString<'local>,
) -> Result<jlong, jni::errors::Error> {
    let target_device_id = jstring_to_string(env, &target_device_id);
    let item_id = jstring_to_string(env, &item_id);

    let Some(engine) = crate::connection_engine_arc(connection_handle) else { return Ok(0) };
    let Ok(runtime) = Runtime::new() else { return Ok(0) };

    let opener: UpstreamOpener = {
        let engine = Arc::clone(&engine);
        let target_device_id = target_device_id.clone();
        Arc::new(move |item_id: String| {
            let engine = Arc::clone(&engine);
            let target_device_id = target_device_id.clone();
            Box::pin(async move {
                engine
                    .open_web_tunnel(&target_device_id, &item_id)
                    .await
                    .map(|handle| Box::new(RealUpstream(handle)) as Box<dyn Upstream>)
            })
        })
    };

    let gateway = match runtime.block_on(web_gateway::start_gateway(item_id, opener, GatewayCaps::default())) {
        Ok(gateway) => gateway,
        Err(error) => {
            tracing::warn!(%error, "web gateway failed to bind");
            return Ok(0);
        }
    };

    Ok(WEB_GATEWAYS.insert(WebGatewaySession { gateway, runtime }))
}

#[allow(clippy::unnecessary_wraps)]
fn native_web_gateway_port(_env: &mut Env<'_>, _class: JClass<'_>, handle: jlong) -> Result<jint, jni::errors::Error> {
    Ok(WEB_GATEWAYS.get(handle, 0, |session| jint::from(session.gateway.port)))
}

fn native_web_gateway_token<'local>(env: &mut Env<'local>, _class: JClass<'local>, handle: jlong) -> Result<JString<'local>, jni::errors::Error> {
    let token = WEB_GATEWAYS.get(handle, String::new(), |session| session.gateway.token.clone());
    JString::new(env, token)
}

/// Stops and unregisters the gateway. Per service-tunnels.md's "closes
/// immediately on unpin" — called from Kotlin's unpin/lifecycle-teardown
/// path, and nowhere does this (or anything reachable from it) send any
/// kind of "stop the remote service" RPC: only the local gateway/tunnel
/// ends, the devhost process is untouched.
#[allow(clippy::unnecessary_wraps)]
fn native_web_gateway_stop(_env: &mut Env<'_>, _class: JClass<'_>, handle: jlong) -> Result<(), jni::errors::Error> {
    if let Some(session) = WEB_GATEWAYS.remove(handle) {
        session.runtime.block_on(session.gateway.stop());
    }
    Ok(())
}

const _WEB_GATEWAY_START: jni::NativeMethod = native_method! {
    java_type = "ai.choosh.WebGatewayBridge",
    static extern fn WebGatewayBridge::native_web_gateway_start(connection_handle: jlong, target_device_id: JString, item_id: JString) -> jlong,
    fn = native_web_gateway_start,
};
const _WEB_GATEWAY_PORT: jni::NativeMethod = native_method! {
    java_type = "ai.choosh.WebGatewayBridge",
    static extern fn WebGatewayBridge::native_web_gateway_port(handle: jlong) -> jint,
    fn = native_web_gateway_port,
};
const _WEB_GATEWAY_TOKEN: jni::NativeMethod = native_method! {
    java_type = "ai.choosh.WebGatewayBridge",
    static extern fn WebGatewayBridge::native_web_gateway_token(handle: jlong) -> JString,
    fn = native_web_gateway_token,
};
const _WEB_GATEWAY_STOP: jni::NativeMethod = native_method! {
    java_type = "ai.choosh.WebGatewayBridge",
    static extern fn WebGatewayBridge::native_web_gateway_stop(handle: jlong) -> void,
    fn = native_web_gateway_stop,
};

// --- MarkdownGatewayBridge -------------------------------------------------

struct MarkdownGatewaySession {
    gateway: markdown_gateway::RunningMarkdownGateway,
    runtime: Runtime,
}

static MARKDOWN_GATEWAYS: LazyLock<HandleRegistry<MarkdownGatewaySession>> = LazyLock::new(HandleRegistry::new);

/// Starts a Markdown loopback gateway for `(workspace_id, doc_path)`,
/// fetching content over the live connection's `"rpc"`-purpose tunnel
/// machinery (`Engine::read_workspace_file_range`) rather than any new
/// tunnel purpose — see `markdown_gateway`'s module doc for the full
/// architecture reasoning.
#[allow(clippy::needless_pass_by_value, clippy::unnecessary_wraps)]
fn native_markdown_gateway_start<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    connection_handle: jlong,
    target_device_id: JString<'local>,
    workspace_id: JString<'local>,
    doc_path: JString<'local>,
) -> Result<jlong, jni::errors::Error> {
    let target_device_id = jstring_to_string(env, &target_device_id);
    let workspace_id = jstring_to_string(env, &workspace_id);
    let doc_path = jstring_to_string(env, &doc_path);

    let Some(engine) = crate::connection_engine_arc(connection_handle) else { return Ok(0) };
    let Ok(runtime) = Runtime::new() else { return Ok(0) };

    let fetcher: FileFetcher = {
        let engine = Arc::clone(&engine);
        let target_device_id = target_device_id.clone();
        let workspace_id = workspace_id.clone();
        Arc::new(move |path: String, range: Option<(u64, u64)>| {
            let engine = Arc::clone(&engine);
            let target_device_id = target_device_id.clone();
            let workspace_id = workspace_id.clone();
            Box::pin(async move {
                match engine.read_workspace_file_range(&target_device_id, &workspace_id, &path, range).await {
                    Ok((content, total_size)) => Ok(FetchedFile { content, total_size }),
                    Err(message) if message.starts_with("not_found") => Err(FetchError::NotFound),
                    Err(message) => Err(FetchError::Other(message)),
                }
            })
        })
    };

    let gateway = match runtime.block_on(markdown_gateway::start_markdown_gateway(doc_path, fetcher, MarkdownGatewayCaps::default()))
    {
        Ok(gateway) => gateway,
        Err(error) => {
            tracing::warn!(%error, "markdown gateway failed to bind");
            return Ok(0);
        }
    };

    Ok(MARKDOWN_GATEWAYS.insert(MarkdownGatewaySession { gateway, runtime }))
}

#[allow(clippy::unnecessary_wraps)]
fn native_markdown_gateway_port(_env: &mut Env<'_>, _class: JClass<'_>, handle: jlong) -> Result<jint, jni::errors::Error> {
    Ok(MARKDOWN_GATEWAYS.get(handle, 0, |session| jint::from(session.gateway.port)))
}

fn native_markdown_gateway_token<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    handle: jlong,
) -> Result<JString<'local>, jni::errors::Error> {
    let token = MARKDOWN_GATEWAYS.get(handle, String::new(), |session| session.gateway.token.clone());
    JString::new(env, token)
}

#[allow(clippy::unnecessary_wraps)]
fn native_markdown_gateway_stop(_env: &mut Env<'_>, _class: JClass<'_>, handle: jlong) -> Result<(), jni::errors::Error> {
    if let Some(session) = MARKDOWN_GATEWAYS.remove(handle) {
        session.runtime.block_on(session.gateway.stop());
    }
    Ok(())
}

/// Test/demo-only: starts a Markdown gateway backed by a fixed in-memory
/// fixture document (a short Markdown source referencing one relative
/// image) rather than a real RPC connection — no `connection_handle`
/// needed at all. Mirrors `terminal_jni.rs`'s `native_terminal_test_inject`
/// precedent ("bypassing the PTY tunnel entirely ... what the on-device
/// verification screenshots ... are driven through"): this is what lets
/// `MarkdownScreen` be verified rendering *real*
/// `choosh-android-bridge::markdown_gateway`/`choosh-web::markdown`
/// output in a real `WebView` on a real device without a live `choosh-hostd`
/// deployment to point at.
#[allow(clippy::unnecessary_wraps)]
fn native_markdown_gateway_start_fixture(_env: &mut Env<'_>, _class: JClass<'_>) -> Result<jlong, jni::errors::Error> {
    let Ok(runtime) = Runtime::new() else { return Ok(0) };

    let markdown_source = "# Choosh Markdown demo\n\n\
        This page is rendered by the **real** `choosh-web::markdown::render_markdown` \
        pipeline, served through the **real** `choosh-android-bridge::markdown_gateway` \
        loopback HTTP server, from a fixed in-memory fixture (no live `choosh-hostd` \
        connection) — the same code path a real workspace document takes, per this \
        module's fixture doc comment.\n\n\
        - a *bulleted* list item\n\
        - and `inline code`\n\n\
        ![a fixture image, served range-capable through /asset](images/fixture.png)\n"
        .to_string();
    // A minimal valid 1x1 red PNG — small enough to embed as a literal, but
    // a real, decodable image `WebView` actually renders (not a stand-in
    // string), proving the `/asset` route's bytes round-trip correctly.
    let fixture_png: Vec<u8> = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=",
    )
    .unwrap_or_default();

    let mut files: HashMap<String, Vec<u8>> = HashMap::new();
    files.insert("fixture.md".to_string(), markdown_source.into_bytes());
    files.insert("images/fixture.png".to_string(), fixture_png);
    let fetcher: FileFetcher = Arc::new(move |path: String, range: Option<(u64, u64)>| {
        let files = files.clone();
        Box::pin(async move {
            let Some(content) = files.get(&path) else { return Err(FetchError::NotFound) };
            let total_size = content.len() as u64;
            match range {
                Some((offset, length)) => {
                    let start = usize::try_from(offset.min(total_size)).unwrap_or(usize::MAX);
                    let end = usize::try_from((offset + length).min(total_size)).unwrap_or(usize::MAX);
                    Ok(FetchedFile { content: content[start..end].to_vec(), total_size })
                }
                None => Ok(FetchedFile { content: content.clone(), total_size }),
            }
        })
    });

    let Ok(gateway) =
        runtime.block_on(markdown_gateway::start_markdown_gateway("fixture.md".to_string(), fetcher, MarkdownGatewayCaps::default()))
    else {
        return Ok(0);
    };
    Ok(MARKDOWN_GATEWAYS.insert(MarkdownGatewaySession { gateway, runtime }))
}

const _MARKDOWN_GATEWAY_START: jni::NativeMethod = native_method! {
    java_type = "ai.choosh.MarkdownGatewayBridge",
    static extern fn MarkdownGatewayBridge::native_markdown_gateway_start(connection_handle: jlong, target_device_id: JString, workspace_id: JString, doc_path: JString) -> jlong,
    fn = native_markdown_gateway_start,
};
const _MARKDOWN_GATEWAY_PORT: jni::NativeMethod = native_method! {
    java_type = "ai.choosh.MarkdownGatewayBridge",
    static extern fn MarkdownGatewayBridge::native_markdown_gateway_port(handle: jlong) -> jint,
    fn = native_markdown_gateway_port,
};
const _MARKDOWN_GATEWAY_TOKEN: jni::NativeMethod = native_method! {
    java_type = "ai.choosh.MarkdownGatewayBridge",
    static extern fn MarkdownGatewayBridge::native_markdown_gateway_token(handle: jlong) -> JString,
    fn = native_markdown_gateway_token,
};
const _MARKDOWN_GATEWAY_STOP: jni::NativeMethod = native_method! {
    java_type = "ai.choosh.MarkdownGatewayBridge",
    static extern fn MarkdownGatewayBridge::native_markdown_gateway_stop(handle: jlong) -> void,
    fn = native_markdown_gateway_stop,
};
const _MARKDOWN_GATEWAY_START_FIXTURE: jni::NativeMethod = native_method! {
    java_type = "ai.choosh.MarkdownGatewayBridge",
    static extern fn MarkdownGatewayBridge::native_markdown_gateway_start_fixture() -> jlong,
    fn = native_markdown_gateway_start_fixture,
};
