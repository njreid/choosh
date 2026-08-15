//! The `WebService` loopback gateway, per `docs/specs/service-tunnels.md`'s
//! "Tunnel" section: an ephemeral, Android-loopback-only HTTP server that
//! cookie-gates access, then bridges each accepted client connection to a
//! `relayd`-brokered `"web:<item_id>"` tunnel (see
//! `choosh_android_transport::PhoneConnection::open_web_tunnel`) reaching
//! the devhost's declared `127.0.0.1:<port>`.
//!
//! **Design: one upstream tunnel per accepted client connection, raw byte
//! pass-through, no HTTP-semantic parsing beyond the very first request's
//! head.** `relayd` "MUST NOT parse or transform the bytes it carries"
//! (DESIGN.md §2.3) and a `"web:<item_id>"` tunnel is functionally identical
//! to a direct TCP connection to the devhost's port (see
//! `open_web_tunnel`'s doc comment) — so the gateway only ever needs to:
//!
//! 1. Accept a loopback TCP connection from the `WebView`.
//! 2. Read just enough to parse the *first* request's head (bounded by
//!    [`GatewayCaps::max_header_bytes`]) and check its `Cookie` header for
//!    the per-pin gateway token — **before opening any tunnel**, per
//!    service-tunnels.md's "never causes a tunnel-open request to relayd at
//!    all" requirement. Missing/wrong token -> `403`, connection closed,
//!    done.
//! 3. Strip the gateway's own cookie pair from that first request's head
//!    (`http_lite::rebuild_head_stripping_cookie`) and open exactly one
//!    fresh upstream tunnel for this connection.
//! 4. From there on, pump raw bytes bidirectionally between the client
//!    socket and the upstream tunnel, applying only byte-level caps (chunk
//!    size, idle timeout, buffered-bytes bound) — no further HTTP parsing.
//!    This is what lets a WebSocket upgrade, an SSE stream, or ordinary
//!    HTTP/1.1 keep-alive reuse all "just work" as the same code path: the
//!    gateway never needs to understand which of those a given connection
//!    is, only that bytes keep flowing.
//!
//! A caveat, stated plainly rather than left implicit: cookie validation
//! happens once per accepted TCP connection (its first request), not once
//! per HTTP request within a kept-alive connection — reparsing every
//! subsequent request on an already-spliced raw byte pipe would require
//! full HTTP framing (`Content-Length`/chunked decoding) this module
//! deliberately doesn't implement (see `http_lite`'s module doc). In
//! practice this is not a gap: the gateway's cookie is `HttpOnly` and set
//! on the `WebView`'s own cookie jar for this loopback origin, so every
//! request the `WebView` makes — including ones reusing a keep-alive
//! connection — already carries it automatically; there is no
//! request-without-the-cookie case this misses in normal operation, only a
//! theoretical "the fourth request on an already-open, already-authenticated
//! connection" case that isn't itself the tunnel-open decision point anyway
//! (that decision was already made once, correctly, for this connection).

// See http_lite.rs's identical attribute/comment: this module's real,
// non-test call site (`gateway_jni.rs`) is android-only.
#![cfg_attr(not(any(test, target_os = "android")), allow(dead_code))]

use crate::http_lite::{
    RunningLoopbackGateway, build_response, cookie_token_matches, must_shutdown, random_token, read_head_with_cap, rebuild_head_stripping_cookie,
};
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, watch};

/// Name of the cookie the gateway sets on the `WebView` and checks on every
/// accepted connection's first request.
pub const TOKEN_COOKIE_NAME: &str = "choosh_gw_token";

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One upstream byte-duplex a gateway connection bridges its client socket
/// to. Implemented for real by [`choosh_android_transport::WebTunnelHandle`]
/// ([`RealUpstream`] below); tests implement it directly over a plain
/// [`TcpStream`] to a fake "devhost" listener, per this crate's verification
/// requirement to prove the gateway's own logic without a real relay
/// connection.
pub trait Upstream: Send {
    fn read(&mut self) -> BoxFuture<'_, Option<Vec<u8>>>;
    fn write(&mut self, bytes: Vec<u8>) -> BoxFuture<'_, Result<(), String>>;
    /// Proactively ends the upstream — called the moment the client
    /// connection ends, so a relay tunnel doesn't linger for its own idle
    /// timeout (service-tunnels.md: "closes immediately on unpin, tunnel
    /// loss, or item removal").
    fn close(self: Box<Self>);
}

// Only constructed by `gateway_jni.rs`'s real `opener` closure — this
// module's own test suite uses `FakeUpstream` instead (see the `tests`
// module below), so this is genuinely unconstructed on a host build/test
// run.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub struct RealUpstream(pub choosh_android_transport::WebTunnelHandle);

impl Upstream for RealUpstream {
    fn read(&mut self) -> BoxFuture<'_, Option<Vec<u8>>> {
        Box::pin(self.0.read())
    }
    fn write(&mut self, bytes: Vec<u8>) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move { self.0.write(&bytes).await.map_err(|error| error.to_string()) })
    }
    fn close(self: Box<Self>) {
        self.0.close();
    }
}

/// Opens one fresh upstream for `item_id` — real callers close over a live
/// `PhoneConnection`/`Engine` and call `open_web_tunnel`; tests close over a
/// fake devhost listener's address and dial it directly.
pub type UpstreamOpener = Arc<dyn Fn(String) -> BoxFuture<'static, Result<Box<dyn Upstream>, String>> + Send + Sync>;

/// Bounds this module picks and the reasoning behind each, per
/// `service-tunnels.md`'s "caps headers, connection count, idle time, and
/// buffered bytes" requirement. None of these are exhaustively tuned (the
/// spec explicitly doesn't ask for that) — each is a deliberately generous
/// but genuinely finite bound for a loopback, single-`WebView`-client dev
/// gateway, not a production internet-facing proxy.
#[derive(Clone, Copy, Debug)]
pub struct GatewayCaps {
    /// A request head (request line + headers) larger than this is rejected
    /// outright. 16 KiB comfortably covers real browser/`WebView` request
    /// headers (cookies, `User-Agent`, `Accept-*`) with headroom, while
    /// still bounding how much of a hostile/broken client's head this
    /// gateway will buffer before giving up (matches common server defaults
    /// in this same order of magnitude, e.g. nginx's `large_client_header_buffers`).
    pub max_header_bytes: usize,
    /// Concurrent accepted connections this gateway serves at once. A
    /// `WebView` loading one dev-server page realistically opens a handful
    /// of parallel connections for assets plus one persistent WebSocket/SSE
    /// stream; 16 gives comfortable headroom for that without allowing
    /// unbounded fan-out from a misbehaving page.
    pub max_concurrent_connections: usize,
    /// A connection with no bytes in either direction for this long is
    /// closed. Long enough that typical SSE/WebSocket keep-alive pings
    /// (commonly every 15-30s) have multiple chances to land first; short
    /// enough to reclaim an abandoned connection reasonably promptly rather
    /// than holding an upstream tunnel open indefinitely.
    pub idle_timeout: Duration,
    /// Maximum bytes read from the client socket per `read()` call in the
    /// upload (client -> upstream) direction — see this module's doc
    /// comment and [`choosh_android_transport::WebTunnelHandle::write`]'s
    /// own doc comment for why this, combined with awaiting each chunk's
    /// upstream `write` before reading the next, is what keeps this
    /// gateway's per-connection memory use bounded to a small, fixed
    /// multiple of this value rather than the size of the request body.
    pub upload_chunk_bytes: usize,
}

impl Default for GatewayCaps {
    fn default() -> Self {
        Self {
            max_header_bytes: 16 * 1024,
            max_concurrent_connections: 16,
            idle_timeout: Duration::from_mins(2),
            upload_chunk_bytes: 16 * 1024,
        }
    }
}

/// This gateway races every in-flight connection against its own shutdown
/// signal (see `accept_loop`'s per-connection `tokio::select!`), so
/// [`RunningLoopbackGateway::stop`]'s "ends the accept loop" guarantee
/// extends here to "and every in-flight connection promptly" — per
/// service-tunnels.md's "closes immediately", callers can rely on the port
/// being free and every connection torn down once `stop()` returns, not
/// just "eventually".
pub type RunningGateway = RunningLoopbackGateway;

/// Starts a gateway for `item_id`, binding an ephemeral port on Android
/// loopback only. `opener` is called once per accepted connection (never
/// before a connection's cookie has been validated — see this module's doc
/// comment) to obtain that connection's upstream.
///
/// # Errors
///
/// [`io::Error`] if the loopback bind itself fails.
pub async fn start_gateway(item_id: String, opener: UpstreamOpener, caps: GatewayCaps) -> io::Result<RunningGateway> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let token = random_token();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let semaphore = Arc::new(Semaphore::new(caps.max_concurrent_connections));

    let accept_token = token.clone();
    let accept_shutdown = shutdown_rx.clone();
    let accept_task = tokio::spawn(async move {
        accept_loop(listener, opener, item_id, accept_token, caps, semaphore, accept_shutdown).await;
    });

    Ok(RunningLoopbackGateway::new(port, token, shutdown_tx, accept_task))
}

#[allow(clippy::too_many_arguments)]
async fn accept_loop(
    listener: TcpListener,
    opener: UpstreamOpener,
    item_id: String,
    token: String,
    caps: GatewayCaps,
    semaphore: Arc<Semaphore>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            () = must_shutdown(&mut shutdown_rx) => return,
            accepted = listener.accept() => {
                let Ok((stream, _peer)) = accepted else { continue };
                // Loopback-only by construction (bound to 127.0.0.1), but a
                // defense-in-depth check costs nothing and documents the
                // invariant explicitly rather than relying solely on the
                // bind address.
                if stream.peer_addr().is_ok_and(|addr| !addr.ip().is_loopback()) {
                    continue;
                }
                let Ok(permit) = Arc::clone(&semaphore).try_acquire_owned() else {
                    // At the concurrent-connection cap: reject immediately
                    // rather than queuing indefinitely, per this gateway's
                    // single-`WebView`-client shape — a client that needs
                    // more than `max_concurrent_connections` in-flight
                    // connections is retried by the `WebView`'s own HTTP
                    // stack like any other transient 503.
                    let mut stream = stream;
                    let _ = stream.write_all(&build_response(503, "Service Unavailable", &[], b"")).await;
                    continue;
                };
                let opener = Arc::clone(&opener);
                let item_id = item_id.clone();
                let token = token.clone();
                let mut connection_shutdown = shutdown_rx.clone();
                let pump_shutdown = shutdown_rx.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    tokio::select! {
                        () = must_shutdown(&mut connection_shutdown) => {}
                        () = serve_connection(stream, opener, item_id, token, caps, pump_shutdown) => {}
                    }
                });
            }
        }
    }
}

/// Serves exactly one accepted client connection end to end: header-gate,
/// open one upstream, then raw bidirectional byte-pump — see this module's
/// doc comment for the full reasoning.
async fn serve_connection(
    mut client: TcpStream,
    opener: UpstreamOpener,
    item_id: String,
    token: String,
    caps: GatewayCaps,
    shutdown_rx: watch::Receiver<bool>,
) {
    let mut buf = Vec::with_capacity(4096);
    let head = match read_head_with_cap(&mut client, &mut buf, caps.max_header_bytes, caps.idle_timeout).await {
        Ok(Some(head)) => head,
        Ok(None) => {
            let _ = client.write_all(&build_response(431, "Request Header Fields Too Large", &[], b"")).await;
            return;
        }
        Err(_) => return,
    };

    if !cookie_token_matches(&head, TOKEN_COOKIE_NAME, &token) {
        // Per service-tunnels.md: 403 here, and — critically — `opener` is
        // never called on this path, so no tunnel-open request reaches
        // relayd for an unauthenticated request.
        let _ = client.write_all(&build_response(403, "Forbidden", &[], b"")).await;
        return;
    }

    let rewritten_head = rebuild_head_stripping_cookie(&head, TOKEN_COOKIE_NAME);
    let leftover = buf[head.head_bytes_len..].to_vec();

    let Ok(mut upstream) = opener(item_id).await else {
        let _ = client.write_all(&build_response(502, "Bad Gateway", &[], b"")).await;
        return;
    };

    if upstream.write(rewritten_head).await.is_err() {
        upstream.close();
        return;
    }
    if !leftover.is_empty() && upstream.write(leftover).await.is_err() {
        upstream.close();
        return;
    }

    pump_bidirectional(client, upstream, caps, shutdown_rx).await;
}

/// Bidirectional raw byte pump between `client` and `upstream`, applying
/// [`GatewayCaps::upload_chunk_bytes`] and [`GatewayCaps::idle_timeout`].
/// Ends (and closes both sides) once both directions have independently
/// signaled done (EOF/close), on any hard error, on a single idle timeout
/// covering *either* direction (reset whenever either makes progress — a
/// one-way stream, e.g. a plain GET response with no further client bytes,
/// must not be killed just because the *other*, quiescent direction alone
/// would look idle), or on `shutdown_rx` firing (unpin/tunnel loss/item
/// removal).
///
/// **Half-close is handled, not treated as "connection over".** A client
/// that finishes sending its request body and calls `shutdown(write)` (or a
/// devhost that finishes its response and closes) only ends *that*
/// direction — per `select!`'s `if` guards below, a finished direction's
/// branch is simply excluded from further polling, while the other
/// direction keeps flowing until it, too, signals done. Treating either
/// EOF as "tear down everything immediately" (an earlier version of this
/// function did exactly that) would truncate a response that was still
/// arriving the moment the client half-closed its own write side — a real,
/// confirmed bug caught by this module's own round-trip test using a
/// half-closing test client.
///
/// Memory bound: the download direction never holds more than one chunk
/// (whatever size the devhost side framed it as, capped upstream at
/// `choosh_protocol::relay::MAX_TUNNEL_FRAME_BYTES` = 256 KiB) — this loop
/// doesn't read the next chunk until the previous one's `write_all` to the
/// client socket completes. The upload direction never holds more than one
/// `upload_chunk_bytes`-sized read — the loop doesn't read the next client
/// chunk until `upstream.write` (which awaits the actual wire send, see
/// `WebTunnelHandle::write`'s doc comment) completes for the previous one.
/// Neither direction ever buffers a whole request/response body.
async fn pump_bidirectional(client: TcpStream, mut upstream: Box<dyn Upstream>, caps: GatewayCaps, mut shutdown_rx: watch::Receiver<bool>) {
    let (mut read_half, mut write_half) = client.into_split();
    let mut upload_buf = vec![0_u8; caps.upload_chunk_bytes];
    let mut client_done = false;
    let mut upstream_done = false;
    let mut last_activity = tokio::time::Instant::now();

    while !(client_done && upstream_done) {
        tokio::select! {
            () = must_shutdown(&mut shutdown_rx) => break,
            () = tokio::time::sleep_until(last_activity + caps.idle_timeout) => break,
            downloaded = upstream.read(), if !upstream_done => {
                if let Some(chunk) = downloaded {
                    last_activity = tokio::time::Instant::now();
                    if write_half.write_all(&chunk).await.is_err() {
                        break;
                    }
                } else {
                    upstream_done = true;
                    let _ = write_half.shutdown().await;
                }
            }
            uploaded = read_half.read(&mut upload_buf), if !client_done => {
                match uploaded {
                    Ok(0) => client_done = true,
                    Ok(read) => {
                        last_activity = tokio::time::Instant::now();
                        if upstream.write(upload_buf[..read].to_vec()).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }
    upstream.close();
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener as TokioTcpListener;

    /// Test-only [`Upstream`]: a plain TCP connection to a fake "devhost"
    /// listener spawned in-process — proves the gateway's own logic (cookie
    /// gate, header rewrite, bidirectional pump, caps) end to end over a
    /// real socket, decoupled from the relay/transport layer entirely, per
    /// this task's verification requirement.
    struct FakeUpstream(TcpStream);

    impl Upstream for FakeUpstream {
        fn read(&mut self) -> BoxFuture<'_, Option<Vec<u8>>> {
            Box::pin(async move {
                let mut buf = [0_u8; 4096];
                match self.0.read(&mut buf).await {
                    Ok(0) | Err(_) => None,
                    Ok(read) => Some(buf[..read].to_vec()),
                }
            })
        }
        fn write(&mut self, bytes: Vec<u8>) -> BoxFuture<'_, Result<(), String>> {
            Box::pin(async move { self.0.write_all(&bytes).await.map_err(|error| error.to_string()) })
        }
        fn close(self: Box<Self>) {}
    }

    /// Spawns a fake devhost that echoes back a canned HTTP response,
    /// recording the raw request bytes it received (including headers) for
    /// the test to assert on. Returns the opener closure and a receiver for
    /// what the fake devhost saw.
    fn fake_opener_recording_requests(response: &'static [u8]) -> (UpstreamOpener, tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let opener: UpstreamOpener = Arc::new(move |_item_id: String| {
            let tx = tx.clone();
            Box::pin(async move {
                let listener = TokioTcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                tokio::spawn(async move {
                    let (mut devhost_stream, _) = listener.accept().await.unwrap();
                    let mut received = Vec::new();
                    let mut buf = [0_u8; 4096];
                    // One read is enough for these small test requests.
                    if let Ok(read) = devhost_stream.read(&mut buf).await {
                        received.extend_from_slice(&buf[..read]);
                    }
                    let _ = tx.send(received);
                    let _ = devhost_stream.write_all(response).await;
                });
                let client = TcpStream::connect(addr).await.map_err(|error| error.to_string())?;
                Ok(Box::new(FakeUpstream(client)) as Box<dyn Upstream>)
            })
        });
        (opener, rx)
    }

    async fn connect_and_send(port: u16, request: &[u8]) -> Vec<u8> {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream.write_all(request).await.unwrap();
        stream.shutdown_write_only().await;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        response
    }

    // `TcpStream` has no `shutdown_write_only` in std tokio; small local
    // helper matching this test module's own naming rather than importing
    // an extension trait for one call site.
    trait ShutdownWriteOnly {
        async fn shutdown_write_only(&mut self);
    }
    impl ShutdownWriteOnly for TcpStream {
        async fn shutdown_write_only(&mut self) {
            let _ = tokio::io::AsyncWriteExt::shutdown(self).await;
        }
    }

    #[tokio::test]
    async fn request_without_the_cookie_gets_403_and_never_opens_an_upstream() {
        let (opener, mut saw_request) = fake_opener_recording_requests(b"unused");
        let gateway = start_gateway("item-1".to_string(), opener, GatewayCaps::default()).await.unwrap();

        let response = connect_and_send(gateway.port, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").await;
        let text = String::from_utf8_lossy(&response);
        assert!(text.starts_with("HTTP/1.1 403"), "{text}");

        // No upstream was ever opened for this request.
        assert!(
            tokio::time::timeout(Duration::from_millis(200), saw_request.recv()).await.is_err(),
            "an upstream tunnel was opened despite the missing cookie"
        );
        gateway.stop().await;
    }

    #[tokio::test]
    async fn request_with_the_wrong_cookie_value_also_gets_403() {
        let (opener, _saw_request) = fake_opener_recording_requests(b"unused");
        let gateway = start_gateway("item-1".to_string(), opener, GatewayCaps::default()).await.unwrap();

        let request = format!("GET / HTTP/1.1\r\nHost: x\r\nCookie: {TOKEN_COOKIE_NAME}=wrong-value\r\n\r\n");
        let response = connect_and_send(gateway.port, request.as_bytes()).await;
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 403"));
        gateway.stop().await;
    }

    #[tokio::test]
    async fn request_with_the_correct_cookie_round_trips_through_a_fake_devhost() {
        let fake_response = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";
        let (opener, mut saw_request) = fake_opener_recording_requests(fake_response);
        let gateway = start_gateway("item-1".to_string(), opener, GatewayCaps::default()).await.unwrap();

        let request = format!(
            "GET /page HTTP/1.1\r\nHost: x\r\nCookie: {}={}; other=kept\r\n\r\n",
            TOKEN_COOKIE_NAME, gateway.token,
        );
        let response = connect_and_send(gateway.port, request.as_bytes()).await;
        assert_eq!(response, fake_response, "response bytes should pass through byte-identical");

        let forwarded = tokio::time::timeout(Duration::from_secs(2), saw_request.recv()).await.unwrap().unwrap();
        let forwarded_text = String::from_utf8_lossy(&forwarded);
        assert!(forwarded_text.starts_with("GET /page HTTP/1.1\r\n"), "{forwarded_text}");
        assert!(!forwarded_text.contains(&gateway.token), "gateway token must be stripped before forwarding upstream");
        assert!(forwarded_text.contains("other=kept"), "a non-gateway cookie pair must survive stripping");
        gateway.stop().await;
    }

    #[tokio::test]
    async fn a_websocket_upgrade_style_exchange_passes_through_unmodified() {
        // No real WebSocket framing needed to prove the point: this
        // gateway never inspects bytes past the first request's head, so a
        // 101 upgrade response followed by arbitrary opaque frame bytes
        // (or, equally, an SSE stream's bytes) passes through exactly like
        // any other response body — the same code path, no special-casing.
        let upgrade_response = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n\x81\x05hello";
        let (opener, _saw_request) = fake_opener_recording_requests(upgrade_response);
        let gateway = start_gateway("item-1".to_string(), opener, GatewayCaps::default()).await.unwrap();

        let request = format!(
            "GET /ws HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nCookie: {}={}\r\n\r\n",
            TOKEN_COOKIE_NAME, gateway.token,
        );
        let response = connect_and_send(gateway.port, request.as_bytes()).await;
        assert_eq!(response, upgrade_response);
        gateway.stop().await;
    }

    #[tokio::test]
    async fn stop_ends_the_accept_loop_promptly() {
        let (opener, _saw_request) = fake_opener_recording_requests(b"unused");
        let gateway = start_gateway("item-1".to_string(), opener, GatewayCaps::default()).await.unwrap();
        let port = gateway.port;
        gateway.stop().await;

        // The port should no longer accept connections in a meaningful way
        // (either refused, or accepted-then-immediately-dropped since the
        // listener itself is gone) — connecting must not hang.
        let outcome = tokio::time::timeout(Duration::from_secs(2), TcpStream::connect(("127.0.0.1", port))).await;
        assert!(outcome.is_ok(), "connecting after stop() should not hang");
    }

    #[tokio::test]
    async fn oversized_header_is_rejected_with_431_not_buffered_unboundedly() {
        let (opener, _saw_request) = fake_opener_recording_requests(b"unused");
        let caps = GatewayCaps { max_header_bytes: 64, ..GatewayCaps::default() };
        let gateway = start_gateway("item-1".to_string(), opener, caps).await.unwrap();

        let huge_header = format!("GET / HTTP/1.1\r\nX-Pad: {}\r\n\r\n", "a".repeat(1024));
        let response = connect_and_send(gateway.port, huge_header.as_bytes()).await;
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 431"));
        gateway.stop().await;
    }
}
