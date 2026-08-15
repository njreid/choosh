//! Choosh's Markdown/Datastar loopback surface, per
//! `docs/specs/service-tunnels.md`'s "`WebView` isolation" section (which
//! names "Choosh's Markdown/Datastar `WebView`" as distinct from a
//! `WebService`'s `WebView`) and
//! `docs/milestones/M5-web-and-markdown.md`'s "remote images/large assets
//! served as root-confined, range-capable loopback URLs".
//!
//! # Architecture decision (stated explicitly — this is the task's one
//! genuinely ambiguous piece)
//!
//! This is a **second, separate** local loopback HTTP server from
//! [`crate::web_gateway`] — different `WebView`, different purpose, not
//! merged — but unlike `web_gateway` it is NOT a byte-transparent tunnel
//! proxy to a devhost port at all. There is no `"web:"`-purpose (or any
//! new) relay tunnel involved here. Instead:
//!
//! `WebView` -> this local server -> `choosh_android_transport::PhoneConnection::call_rpc`
//! (the *existing*, already-built `"rpc"`-purpose tunnel machinery) ->
//! `choosh-hostd` -> `workspace.file.read`.
//!
//! Reasoning: `choosh-web::markdown::render_markdown` is a pure,
//! already-tested function (source string + `AssetResolver` in, sanitized
//! HTML out) with "no listener or browser bridge yet" by its own module
//! doc. The only two ways to get a rendered page in front of a `WebView`
//! are (a) `choosh-hostd` renders server-side and serves HTML over a new
//! tunnel purpose, or (b) Android fetches raw Markdown source over the
//! *already-existing* RPC tunnel and renders it locally. This module picks
//! (b), because:
//!
//! - It needs zero new relay/`hostd` wire surface: `workspace.file.read`
//!   already exists (M1), already supports `revision`/`range`
//!   (`jj-integration.md`), and `choosh-hostd`'s RPC dispatch needs no new
//!   case at all — `render_markdown` is a client-side concern.
//! - It matches this system's stated split precisely: "relay is a blind
//!   broker, `hostd` does real work, Android renders"
//!   (`docs/specs/relay-protocol.md`'s framing, reflected throughout
//!   DESIGN.md) — rendering Markdown to HTML is exactly the kind of
//!   presentation work Android already does for every other item type
//!   (diff hunks, change-graph nodes) rather than asking `hostd` to hand
//!   back pre-rendered UI.
//! - `choosh-web` was built with exactly this seam: its crate doc says the
//!   renderer is "decoupled ... that increment will wire up to a real file
//!   source" — i.e. *some* caller fetches content and hands it to
//!   `render_markdown`; nothing in `choosh-web` assumes that caller is
//!   `choosh-hostd` itself, and using it directly from
//!   `choosh-android-bridge` (a plain `rlib`/non-JNI-specific dependency,
//!   same shape as `choosh-terminal-engine`'s existing precedent) is a
//!   one-line `Cargo.toml` addition.
//! - It reuses `PhoneConnection::call_rpc` verbatim rather than inventing
//!   any new transport, keeping every RPC call sandboxed to the fresh-
//!   tunnel-per-call discipline `call_rpc` already documents.
//!
//! The "range-capable loopback URL" requirement is satisfied by this
//! module's own `/asset` route translating an HTTP `Range` header (or, if
//! absent, an internally chosen bounded first-chunk read) into
//! `workspace.file.read`'s `range: {offset, length}` parameter — never
//! reading a whole large asset into memory or inlining it as a base64 data
//! URI. `host-rpc.md`'s stated per-request range bound (4 MiB) is this
//! module's own [`MAX_ASSET_CHUNK_BYTES`].

// See http_lite.rs's identical attribute/comment: this module's real,
// non-test call site (`gateway_jni.rs`) is android-only.
#![cfg_attr(not(any(test, target_os = "android")), allow(dead_code))]

use crate::http_lite::{RequestHead, build_response, try_parse_head};
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, watch};

pub const TOKEN_COOKIE_NAME: &str = "choosh_md_token";

/// Per `host-rpc.md`: "`workspace.file.read` range: 4 MiB per request;
/// larger files require multiple ranged requests" — this module's own
/// per-`/asset`-request chunk bound, so a single RPC round trip (and this
/// gateway's own per-response memory use) never exceeds that regardless of
/// how large the underlying asset is.
pub const MAX_ASSET_CHUNK_BYTES: u64 = 4 * 1024 * 1024;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug)]
pub enum FetchError {
    NotFound,
    // Only constructed by `gateway_jni.rs`'s real fetcher (a transport-level
    // `Engine::read_workspace_file_range` failure that isn't a clean
    // not-found) — no test fixture in this module's own suite needs a
    // non-`NotFound` failure case, so this is genuinely unconstructed on a
    // host build/test run, same as `RealUpstream` below.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    Other(String),
}

pub struct FetchedFile {
    pub content: Vec<u8>,
    pub total_size: u64,
}

/// Fetches `path` (workspace-relative) at optional byte `range`
/// `(offset, length)`. Real callers close over a live `Engine`/
/// `PhoneConnection` and issue `RpcRequest::WorkspaceFileRead`; tests close
/// over canned in-memory fixtures, decoupled from any live RPC connection.
pub type FileFetcher = Arc<dyn Fn(String, Option<(u64, u64)>) -> BoxFuture<'static, Result<FetchedFile, FetchError>> + Send + Sync>;

#[derive(Clone, Copy, Debug)]
pub struct MarkdownGatewayCaps {
    /// Same reasoning as `web_gateway::GatewayCaps::max_header_bytes`.
    pub max_header_bytes: usize,
    /// Smaller than `web_gateway`'s: this server only ever serves the
    /// single Markdown document view plus its own referenced assets, not
    /// an arbitrary dev-server page's full asset graph.
    pub max_concurrent_connections: usize,
    pub idle_timeout: Duration,
}

impl Default for MarkdownGatewayCaps {
    fn default() -> Self {
        Self { max_header_bytes: 16 * 1024, max_concurrent_connections: 8, idle_timeout: Duration::from_secs(30) }
    }
}

pub struct RunningMarkdownGateway {
    pub port: u16,
    pub token: String,
    shutdown_tx: watch::Sender<bool>,
    accept_task: tokio::task::JoinHandle<()>,
}

impl RunningMarkdownGateway {
    pub async fn stop(self) {
        let _ = self.shutdown_tx.send(true);
        let _ = self.accept_task.await;
    }
}

/// Starts a Markdown gateway bound to one workspace document context: every
/// `/doc`/`/asset` request this instance serves is implicitly scoped to
/// whatever `(target_device_id, workspace_id)` `fetcher` itself was built
/// against — this module never sees or threads those ids through the
/// `WebView`-visible URL surface at all, matching
/// `docs/milestones/M5-web-and-markdown.md`'s "without the `WebView` ever
/// holding a devhost address or credential".
///
/// # Errors
///
/// [`io::Error`] if the loopback bind fails.
pub async fn start_markdown_gateway(
    doc_path: String,
    fetcher: FileFetcher,
    caps: MarkdownGatewayCaps,
) -> io::Result<RunningMarkdownGateway> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let token = random_token();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let semaphore = Arc::new(Semaphore::new(caps.max_concurrent_connections));

    let accept_token = token.clone();
    let accept_task = tokio::spawn(accept_loop(listener, doc_path, fetcher, accept_token, caps, semaphore, shutdown_rx));

    Ok(RunningMarkdownGateway { port, token, shutdown_tx, accept_task })
}

#[allow(clippy::too_many_arguments)]
async fn accept_loop(
    listener: TcpListener,
    doc_path: String,
    fetcher: FileFetcher,
    token: String,
    caps: MarkdownGatewayCaps,
    semaphore: Arc<Semaphore>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            () = must_shutdown(&mut shutdown_rx) => return,
            accepted = listener.accept() => {
                let Ok((mut stream, peer)) = accepted else { continue };
                if !peer.ip().is_loopback() {
                    continue;
                }
                let Ok(permit) = Arc::clone(&semaphore).try_acquire_owned() else {
                    let _ = stream.write_all(&build_response(503, "Service Unavailable", &[], b"")).await;
                    continue;
                };
                let doc_path = doc_path.clone();
                let fetcher = Arc::clone(&fetcher);
                let token = token.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    serve_connection(stream, doc_path, fetcher, token, caps).await;
                });
            }
        }
    }
}

async fn must_shutdown(rx: &mut watch::Receiver<bool>) {
    loop {
        if *rx.borrow() {
            return;
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}

async fn serve_connection(
    mut stream: tokio::net::TcpStream,
    doc_path: String,
    fetcher: FileFetcher,
    token: String,
    caps: MarkdownGatewayCaps,
) {
    let mut buf = Vec::with_capacity(2048);
    let Some(head) = read_head_with_cap(&mut stream, &mut buf, caps.max_header_bytes, caps.idle_timeout).await else {
        let _ = stream.write_all(&build_response(431, "Request Header Fields Too Large", &[], b"")).await;
        return;
    };

    let cookie_ok = head
        .header("cookie")
        .map(crate::http_lite::parse_cookie_header)
        .is_some_and(|pairs| pairs.iter().any(|(name, value)| name == TOKEN_COOKIE_NAME && value == &token));
    if !cookie_ok {
        let _ = stream.write_all(&build_response(403, "Forbidden", &[], b"")).await;
        return;
    }

    let (route, query) = split_target(&head.target);
    let response = match route {
        "/doc" => render_doc_response(&doc_path, &fetcher).await,
        "/asset" => serve_asset_response(&query, head.header("range"), &fetcher).await,
        _ => build_response(404, "Not Found", &[], b"not found"),
    };
    let _ = stream.write_all(&response).await;
}

async fn read_head_with_cap(
    stream: &mut tokio::net::TcpStream,
    buf: &mut Vec<u8>,
    max_header_bytes: usize,
    idle_timeout: Duration,
) -> Option<RequestHead> {
    let mut chunk = [0_u8; 2048];
    loop {
        // See web_gateway::read_head_with_cap's identical ordering note:
        // the cap must be checked before parsing, not after, so a head
        // that arrives oversized-but-complete in a single read is still
        // rejected.
        if buf.len() > max_header_bytes {
            return None;
        }
        if let Ok(Some(head)) = try_parse_head(buf) {
            return Some(head);
        }
        let Ok(Ok(read)) = tokio::time::timeout(idle_timeout, stream.read(&mut chunk)).await else { return None };
        if read == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..read]);
    }
}

async fn render_doc_response(doc_path: &str, fetcher: &FileFetcher) -> Vec<u8> {
    match fetcher(doc_path.to_string(), None).await {
        Ok(file) => {
            let Ok(source) = String::from_utf8(file.content) else {
                return build_response(415, "Unsupported Media Type", &[], b"not valid UTF-8 markdown");
            };
            let doc_dir = parent_dir(doc_path);
            let resolver = AssetUrlResolver { doc_dir };
            match choosh_web::markdown::render_markdown(&source, &resolver) {
                Ok(html) => {
                    let page = wrap_page(&html);
                    build_response(200, "OK", &[("Content-Type", "text/html; charset=utf-8".to_string())], page.as_bytes())
                }
                Err(error) => build_response(500, "Internal Server Error", &[], error.to_string().as_bytes()),
            }
        }
        Err(FetchError::NotFound) => build_response(404, "Not Found", &[], b"not found"),
        Err(FetchError::Other(message)) => build_response(502, "Bad Gateway", &[], message.as_bytes()),
    }
}

/// Wraps rendered Markdown HTML in a minimal page shell with no scripts, no
/// bridge object, and no external resource references beyond this same
/// loopback origin's own `/asset` route — matching this `WebView`'s
/// isolation posture (no JS bridge reachable, nothing to bridge to).
fn wrap_page(body_html: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <style>body{{font-family:sans-serif;max-width:48rem;margin:1.5rem auto;padding:0 1rem;line-height:1.5;}}\
         img{{max-width:100%;}}pre{{overflow-x:auto;background:#f4f4f4;padding:0.75rem;}}</style></head>\
         <body>{body_html}</body></html>"
    )
}

async fn serve_asset_response(query: &str, range_header: Option<&str>, fetcher: &FileFetcher) -> Vec<u8> {
    let params = parse_query(query);
    let Some(path) = params.get("path").cloned() else {
        return build_response(400, "Bad Request", &[], b"missing path");
    };

    let requested_range = range_header.and_then(parse_range_header);
    let client_asked_for_range = requested_range.is_some();
    let (offset, length) = requested_range.unwrap_or((0, MAX_ASSET_CHUNK_BYTES));
    let length = length.min(MAX_ASSET_CHUNK_BYTES);

    match fetcher(path, Some((offset, length))).await {
        Ok(file) => {
            let end = offset + file.content.len() as u64;
            let content_type = guess_content_type(&params.get("path").cloned().unwrap_or_default());
            if client_asked_for_range || end < file.total_size {
                let headers = [
                    ("Content-Type", content_type),
                    ("Accept-Ranges", "bytes".to_string()),
                    ("Content-Range", format!("bytes {}-{}/{}", offset, end.saturating_sub(1).max(offset), file.total_size)),
                ];
                build_response(206, "Partial Content", &headers, &file.content)
            } else {
                let headers = [("Content-Type", content_type), ("Accept-Ranges", "bytes".to_string())];
                build_response(200, "OK", &headers, &file.content)
            }
        }
        Err(FetchError::NotFound) => build_response(404, "Not Found", &[], b"not found"),
        Err(FetchError::Other(message)) => build_response(502, "Bad Gateway", &[], message.as_bytes()),
    }
}

/// `render_markdown`'s [`choosh_web::markdown::AssetResolver`]: rewrites a
/// safe workspace-relative reference (already gated by
/// `choosh_web::is_safe_relative_asset` before this is even called) into
/// this gateway's own `/asset?path=...` loopback URL — never a devhost
/// address, never inlined as a data URI.
struct AssetUrlResolver {
    doc_dir: String,
}

impl choosh_web::markdown::AssetResolver for AssetUrlResolver {
    fn resolve(&self, reference: &str) -> Option<String> {
        let resolved_path = if self.doc_dir.is_empty() { reference.to_string() } else { format!("{}/{reference}", self.doc_dir) };
        Some(format!("/asset?path={}", percent_encode(&resolved_path)))
    }
}

fn parent_dir(path: &str) -> String {
    path.rsplit_once('/').map(|(dir, _file)| dir.to_string()).unwrap_or_default()
}

fn split_target(target: &str) -> (&str, String) {
    match target.split_once('?') {
        Some((route, query)) => (route, query.to_string()),
        None => (target, String::new()),
    }
}

fn parse_query(query: &str) -> std::collections::HashMap<String, String> {
    query
        .split('&')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            Some((percent_decode(key), percent_decode(value)))
        })
        .collect()
}

/// Parses `Range: bytes=<start>-<end>` (the only form this gateway needs to
/// honor — a single, closed range) into `(offset, length)`. Anything else
/// (multi-range, suffix-range, malformed) is treated as "no range given",
/// which falls back to this module's own bounded default chunk.
fn parse_range_header(value: &str) -> Option<(u64, u64)> {
    let spec = value.strip_prefix("bytes=")?;
    let (start, end) = spec.split_once('-')?;
    let start: u64 = start.parse().ok()?;
    let end: u64 = end.parse().ok()?;
    if end < start {
        return None;
    }
    Some((start, end - start + 1))
}

fn guess_content_type(path: &str) -> String {
    let extension = path.rsplit_once('.').map_or(String::new(), |(_, ext)| ext.to_ascii_lowercase());
    match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
    .to_string()
}

fn percent_encode(value: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => out.push(byte as char),
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(decoded) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
        {
            out.push(decoded);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn random_token() -> String {
    use std::io::Read;
    let mut bytes = [0_u8; 32];
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        let _ = file.read_exact(&mut bytes);
    }
    crate::http_lite::hex_encode(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpStream;

    fn fixed_fetcher(files: std::collections::HashMap<String, Vec<u8>>) -> FileFetcher {
        Arc::new(move |path: String, range: Option<(u64, u64)>| {
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
        })
    }

    async fn connect_and_send(port: u16, request: &[u8]) -> Vec<u8> {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream.write_all(request).await.unwrap();
        let _ = tokio::io::AsyncWriteExt::shutdown(&mut stream).await;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        response
    }

    #[tokio::test]
    async fn doc_request_without_the_cookie_is_forbidden() {
        let files = std::collections::HashMap::from([("README.md".to_string(), b"# hi".to_vec())]);
        let gateway =
            start_markdown_gateway("README.md".to_string(), fixed_fetcher(files), MarkdownGatewayCaps::default()).await.unwrap();
        let response = connect_and_send(gateway.port, b"GET /doc HTTP/1.1\r\nHost: x\r\n\r\n").await;
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 403"));
        gateway.stop().await;
    }

    #[tokio::test]
    async fn doc_request_renders_markdown_to_html() {
        let files = std::collections::HashMap::from([("README.md".to_string(), b"# Title\n\nHello *world*.\n".to_vec())]);
        let gateway =
            start_markdown_gateway("README.md".to_string(), fixed_fetcher(files), MarkdownGatewayCaps::default()).await.unwrap();
        let request = format!("GET /doc HTTP/1.1\r\nHost: x\r\nCookie: {}={}\r\n\r\n", TOKEN_COOKIE_NAME, gateway.token);
        let response = connect_and_send(gateway.port, request.as_bytes()).await;
        let text = String::from_utf8_lossy(&response);
        assert!(text.starts_with("HTTP/1.1 200 OK"), "{text}");
        assert!(text.contains("<h1>Title</h1>"), "{text}");
        assert!(text.contains("<em>world</em>"), "{text}");
        assert!(!text.contains("<script"), "markdown page must carry no scripts: {text}");
        gateway.stop().await;
    }

    #[tokio::test]
    async fn relative_image_reference_is_rewritten_to_a_loopback_asset_url() {
        let files = std::collections::HashMap::from([
            ("docs/README.md".to_string(), b"![logo](images/logo.png)\n".to_vec()),
            ("docs/images/logo.png".to_string(), vec![1, 2, 3, 4]),
        ]);
        let gateway =
            start_markdown_gateway("docs/README.md".to_string(), fixed_fetcher(files), MarkdownGatewayCaps::default())
                .await
                .unwrap();
        let request = format!("GET /doc HTTP/1.1\r\nHost: x\r\nCookie: {}={}\r\n\r\n", TOKEN_COOKIE_NAME, gateway.token);
        let response = connect_and_send(gateway.port, request.as_bytes()).await;
        let text = String::from_utf8_lossy(&response);
        assert!(text.contains("src=\"/asset?path=docs/images/logo.png\""), "{text}");
        gateway.stop().await;
    }

    #[tokio::test]
    async fn asset_request_without_a_range_header_returns_the_whole_small_file_as_200() {
        let files = std::collections::HashMap::from([("images/logo.png".to_string(), vec![9_u8; 100])]);
        let gateway =
            start_markdown_gateway("README.md".to_string(), fixed_fetcher(files), MarkdownGatewayCaps::default()).await.unwrap();
        let request = format!(
            "GET /asset?path=images%2Flogo.png HTTP/1.1\r\nHost: x\r\nCookie: {}={}\r\n\r\n",
            TOKEN_COOKIE_NAME, gateway.token,
        );
        let response = connect_and_send(gateway.port, request.as_bytes()).await;
        let text = String::from_utf8_lossy(&response);
        assert!(text.starts_with("HTTP/1.1 200 OK"), "{text}");
        assert!(text.contains("Content-Type: image/png"), "{text}");
        assert!(response.ends_with(&[9_u8; 100][..]));
        gateway.stop().await;
    }

    #[tokio::test]
    async fn asset_request_for_a_file_larger_than_one_chunk_returns_206_with_content_range() {
        let big = vec![7_u8; usize::try_from(MAX_ASSET_CHUNK_BYTES + 1000).unwrap()];
        let files = std::collections::HashMap::from([("big.png".to_string(), big.clone())]);
        let gateway =
            start_markdown_gateway("README.md".to_string(), fixed_fetcher(files), MarkdownGatewayCaps::default()).await.unwrap();
        let request = format!(
            "GET /asset?path=big.png HTTP/1.1\r\nHost: x\r\nCookie: {}={}\r\n\r\n",
            TOKEN_COOKIE_NAME, gateway.token,
        );
        let response = connect_and_send(gateway.port, request.as_bytes()).await;
        let text_prefix = String::from_utf8_lossy(&response[..300]);
        assert!(text_prefix.starts_with("HTTP/1.1 206 Partial Content"), "{text_prefix}");
        assert!(text_prefix.contains(&format!("Content-Range: bytes 0-{}/{}", MAX_ASSET_CHUNK_BYTES - 1, big.len())), "{text_prefix}");
        gateway.stop().await;
    }

    #[tokio::test]
    async fn explicit_range_header_is_honored_and_translated() {
        let files = std::collections::HashMap::from([("f.bin".to_string(), (0_u8..=255).collect::<Vec<u8>>())]);
        let gateway =
            start_markdown_gateway("README.md".to_string(), fixed_fetcher(files), MarkdownGatewayCaps::default()).await.unwrap();
        let request = format!(
            "GET /asset?path=f.bin HTTP/1.1\r\nHost: x\r\nRange: bytes=10-19\r\nCookie: {}={}\r\n\r\n",
            TOKEN_COOKIE_NAME, gateway.token,
        );
        let response = connect_and_send(gateway.port, request.as_bytes()).await;
        let (head_bytes, body) = split_response(&response);
        let head_text = String::from_utf8_lossy(head_bytes);
        assert!(head_text.starts_with("HTTP/1.1 206 Partial Content"), "{head_text}");
        assert!(head_text.contains("Content-Range: bytes 10-19/256"), "{head_text}");
        assert_eq!(body, (10_u8..=19).collect::<Vec<u8>>());
        gateway.stop().await;
    }

    #[tokio::test]
    async fn a_missing_file_is_a_clean_404_not_a_hang_or_panic() {
        let gateway = start_markdown_gateway(
            "README.md".to_string(),
            fixed_fetcher(std::collections::HashMap::new()),
            MarkdownGatewayCaps::default(),
        )
        .await
        .unwrap();
        let request = format!("GET /doc HTTP/1.1\r\nHost: x\r\nCookie: {}={}\r\n\r\n", TOKEN_COOKIE_NAME, gateway.token);
        let response = connect_and_send(gateway.port, request.as_bytes()).await;
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 404"));
        gateway.stop().await;
    }

    fn split_response(response: &[u8]) -> (&[u8], &[u8]) {
        let boundary = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        (&response[..boundary + 4], &response[boundary + 4..])
    }
}
