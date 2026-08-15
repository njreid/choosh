//! A minimal, dependency-free HTTP/1.x head parser and response writer,
//! shared by [`crate::web_gateway`] and [`crate::markdown_gateway`].
//!
//! **Why not a real HTTP server crate (hyper etc.)?** Both gateways this
//! module backs are loopback-only, single-purpose, and — per
//! `docs/specs/service-tunnels.md`'s "Tunnel" section — need to treat the
//! request/response *body* as opaque bytes to pass through untouched
//! (`web_gateway`) or to fully own end to end (`markdown_gateway`'s own
//! small JSON/HTML responses), never as a shape a general-purpose server
//! needs to validate against arbitrary handlers, content negotiation, or
//! HTTP/2. Parsing just the request line and headers (bounded, capped) and
//! writing a status line/headers/body by hand is a few dozen lines and
//! keeps every cap (header size, timeouts) directly visible in this crate
//! rather than configured through a general framework's own knobs. A real
//! server crate would still need all the same bespoke plumbing
//! (byte-transparent body passthrough for `web_gateway`, a tiny 2-route
//! table for `markdown_gateway`) on top, for no real savings here.
//!
//! This module deliberately does NOT parse `Transfer-Encoding: chunked` or
//! track `Content-Length` for [`crate::web_gateway`]'s use — it only needs
//! to find the head/body boundary of the *first* request on a freshly
//! accepted connection (to inspect/rewrite the `Cookie` header before
//! anything is forwarded upstream); everything after that boundary is
//! passed through as raw bytes without this module caring what protocol
//! (HTTP keep-alive, a WebSocket upgrade, an SSE stream) they represent.

// This module's real, non-test call sites are `web_gateway`/
// `markdown_gateway`, both wired up to JNI in `gateway_jni.rs` — which is
// `#[cfg(target_os = "android")]`-gated, per this crate's existing
// precedent (see `engine.rs`'s `open_pty_tunnel` doc comment) for "a host
// build legitimately never calls this despite it being real, tested
// (via this crate's Android-target build) production code."
#![cfg_attr(not(any(test, target_os = "android")), allow(dead_code))]

use std::io;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::sync::watch;

/// A parsed HTTP/1.x request (or response) head: the start line plus
/// headers, with `header_bytes_len` marking exactly where the blank line
/// terminating the head ends in the original buffer — bytes from there
/// onward are body/payload, untouched by this parser.
#[derive(Debug, Clone)]
pub struct RequestHead {
    pub method: String,
    pub target: String,
    pub version: String,
    /// In original order, lowercased names — case-insensitive lookup via
    /// [`Self::header`], but original values preserved verbatim.
    pub headers: Vec<(String, String)>,
    pub head_bytes_len: usize,
}

impl RequestHead {
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
    }
}

/// Scans `buf` for the blank line (`\r\n\r\n`) terminating an HTTP head and,
/// if found, parses the request line and headers preceding it.
///
/// Returns `Ok(None)` if the head is not yet fully buffered (the caller
/// should read more, subject to its own header-size cap — this function
/// does not itself cap `buf`'s length). Returns `Err` for a request line or
/// header line that doesn't parse as HTTP/1.x at all — a genuinely
/// malformed peer, not a truncation-in-progress case.
///
/// # Errors
///
/// [`io::Error`] with `InvalidData` if the head is present but malformed.
pub fn try_parse_head(buf: &[u8]) -> Result<Option<RequestHead>, io::Error> {
    let Some(boundary) = find_double_crlf(buf) else { return Ok(None) };
    let head_bytes = &buf[..boundary];
    let head_str = std::str::from_utf8(head_bytes).map_err(|_| invalid("head is not valid UTF-8"))?;
    let mut lines = head_str.split("\r\n");
    let request_line = lines.next().ok_or_else(|| invalid("missing request line"))?;
    let mut parts = request_line.splitn(3, ' ');
    let method = parts.next().ok_or_else(|| invalid("missing method"))?.to_string();
    let target = parts.next().ok_or_else(|| invalid("missing target"))?.to_string();
    let version = parts.next().ok_or_else(|| invalid("missing version"))?.to_string();

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line.split_once(':').ok_or_else(|| invalid("malformed header line"))?;
        headers.push((name.trim().to_string(), value.trim().to_string()));
    }

    Ok(Some(RequestHead { method, target, version, headers, head_bytes_len: boundary + 4 }))
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|window| window == b"\r\n\r\n")
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

/// Reads from `stream` into `buf` until a full HTTP head is present (per
/// [`try_parse_head`]), the header cap is exceeded (`Ok(None)`), the
/// connection ends, or it idles past `idle_timeout` (both `Err`).
///
/// Shared by [`crate::web_gateway`]/[`crate::markdown_gateway`]'s otherwise
/// byte-for-byte identical "read until one complete head or give up" loop —
/// every cap/timeout/status-code decision (what to do with `Ok(None)` vs
/// `Err`) stays in each gateway's own `serve_connection`, since that's
/// where the two genuinely diverge: `web_gateway` sends a `431` only for
/// the oversized case and otherwise closes silently (a still-in-progress
/// truncated head from a client that simply disconnected isn't a "your
/// headers were too large" error), which is the response this function's
/// distinct `Ok(None)`/`Err` cases are shaped to let each caller apply
/// correctly (`markdown_gateway`'s pre-extraction version conflated both
/// into a single `431`, which this shared version now fixes for it too).
///
/// # Errors
///
/// [`io::Error`] with `TimedOut` on an idle timeout, `UnexpectedEof` if the
/// connection ends before a full head arrives, or `InvalidData` if what's
/// buffered so far doesn't parse as a well-formed head at all.
pub async fn read_head_with_cap<S>(stream: &mut S, buf: &mut Vec<u8>, max_header_bytes: usize, idle_timeout: Duration) -> io::Result<Option<RequestHead>>
where
    S: tokio::io::AsyncRead + Unpin,
{
    let mut chunk = [0_u8; 4096];
    loop {
        // Checked before parsing, not after: a single read can pull in
        // more bytes than the cap in one shot (a small/fast client), and a
        // head that happens to already be complete at that point must
        // still be rejected as oversized rather than accepted just because
        // it arrived in one piece.
        if buf.len() > max_header_bytes {
            return Ok(None);
        }
        if let Some(head) = try_parse_head(buf)? {
            return Ok(Some(head));
        }
        let read = tokio::time::timeout(idle_timeout, stream.read(&mut chunk))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "idle"))??;
        if read == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "client closed before a full head arrived"));
        }
        buf.extend_from_slice(&chunk[..read]);
    }
}

/// Waits until `rx.borrow()` becomes `true` (or the sender is dropped, which
/// this treats the same as a shutdown signal). Shared by
/// [`crate::web_gateway`]/[`crate::markdown_gateway`]'s identical accept-loop
/// and per-connection shutdown races.
pub async fn must_shutdown(rx: &mut watch::Receiver<bool>) {
    loop {
        if *rx.borrow() {
            return;
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}

/// A live loopback gateway: bound to an OS-assigned loopback port, gating on
/// a per-instance `token`. Dropping this (or calling [`Self::stop`]) ends
/// the accept loop; each gateway's own per-connection shutdown handling
/// (see its own `accept_loop`) determines how promptly in-flight
/// connections themselves end — see [`crate::web_gateway::RunningGateway`]/
/// [`crate::markdown_gateway::RunningMarkdownGateway`], both a bare type
/// alias to this struct: the two gateways differ in what each accepted
/// connection does, never in how the gateway itself is started, addressed,
/// or stopped.
pub struct RunningLoopbackGateway {
    pub port: u16,
    pub token: String,
    shutdown_tx: watch::Sender<bool>,
    accept_task: tokio::task::JoinHandle<()>,
}

impl RunningLoopbackGateway {
    #[must_use]
    pub fn new(port: u16, token: String, shutdown_tx: watch::Sender<bool>, accept_task: tokio::task::JoinHandle<()>) -> Self {
        Self { port, token, shutdown_tx, accept_task }
    }

    /// Signals the accept loop (and, for gateways that race their own
    /// per-connection tasks against the same signal) every in-flight
    /// connection to end, then waits for the accept loop itself to
    /// actually stop.
    pub async fn stop(self) {
        let _ = self.shutdown_tx.send(true);
        let _ = self.accept_task.await;
    }
}

/// Parses a `Cookie` header value (`name1=value1; name2=value2`) into pairs.
#[must_use]
pub fn parse_cookie_header(value: &str) -> Vec<(String, String)> {
    value
        .split(';')
        .filter_map(|pair| {
            let pair = pair.trim();
            let (name, value) = pair.split_once('=')?;
            Some((name.trim().to_string(), value.trim().to_string()))
        })
        .collect()
}

/// Checks whether `head`'s `Cookie` header carries a pair named
/// `cookie_name` with value `token` — the cookie-gate check
/// [`crate::web_gateway`]/[`crate::markdown_gateway`] both apply to a
/// connection's first request, and *only* that check, before doing anything
/// that reaches past this loopback gateway (opening a tunnel, issuing an
/// RPC fetch) — see either module's doc comment for why that ordering is
/// load-bearing, not incidental.
#[must_use]
pub fn cookie_token_matches(head: &RequestHead, cookie_name: &str, token: &str) -> bool {
    head.header("cookie").map(parse_cookie_header).is_some_and(|pairs| pairs.iter().any(|(name, value)| name == cookie_name && value == token))
}

/// Rebuilds the raw request-line + header bytes for `head`, with any cookie
/// pair named `strip_cookie_name` removed from a `Cookie` header (dropping
/// the header entirely if that was its only pair) — per
/// `service-tunnels.md`'s "strips the gateway cookie before forwarding".
/// Every other header (including a `Cookie` header with no matching pair)
/// passes through byte-identical.
#[must_use]
pub fn rebuild_head_stripping_cookie(head: &RequestHead, strip_cookie_name: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(head.head_bytes_len);
    out.extend_from_slice(head.method.as_bytes());
    out.push(b' ');
    out.extend_from_slice(head.target.as_bytes());
    out.push(b' ');
    out.extend_from_slice(head.version.as_bytes());
    out.extend_from_slice(b"\r\n");
    for (name, value) in &head.headers {
        if name.eq_ignore_ascii_case("cookie") {
            let remaining: Vec<String> = parse_cookie_header(value)
                .into_iter()
                .filter(|(cookie_name, _)| cookie_name != strip_cookie_name)
                .map(|(cookie_name, cookie_value)| format!("{cookie_name}={cookie_value}"))
                .collect();
            if remaining.is_empty() {
                continue;
            }
            out.extend_from_slice(b"Cookie: ");
            out.extend_from_slice(remaining.join("; ").as_bytes());
            out.extend_from_slice(b"\r\n");
            continue;
        }
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\r\n");
    out
}

/// Lowercase-hex-encodes `bytes`.
#[must_use]
pub fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// Generates a fresh per-instance gateway auth token: 32 bytes (256 bits)
/// read from `/dev/urandom`, hex-encoded so it's a safe, unambiguous cookie
/// value with no escaping concerns. Shared by
/// [`crate::web_gateway`]/[`crate::markdown_gateway`]'s identical token
/// generation (no other need for a `rand` dependency at this crate's small,
/// occasional-use call sites).
#[must_use]
pub fn random_token() -> String {
    use std::io::Read;
    let mut bytes = [0_u8; 32];
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        let _ = file.read_exact(&mut bytes);
    }
    hex_encode(&bytes)
}

/// Builds a minimal, complete HTTP/1.1 response: status line, `headers`
/// verbatim, `Content-Length` derived from `body`, and the body itself.
/// Every extra header [`crate::web_gateway`]/[`crate::markdown_gateway`]
/// send (`Set-Cookie`, `Content-Type`, `Accept-Ranges`, `Content-Range`) is
/// passed in by the caller rather than special-cased here.
#[must_use]
pub fn build_response(status: u16, reason: &str, headers: &[(&str, String)], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 256);
    out.extend_from_slice(format!("HTTP/1.1 {status} {reason}\r\n").as_bytes());
    for (name, value) in headers {
        out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    out.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
    out.extend_from_slice(b"Connection: close\r\n");
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(body);
    out
}

#[cfg(test)]
mod tests {
    use super::{
        RunningLoopbackGateway, build_response, cookie_token_matches, hex_encode, must_shutdown, parse_cookie_header, random_token, read_head_with_cap,
        rebuild_head_stripping_cookie, try_parse_head,
    };
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::watch;

    #[test]
    fn incomplete_head_returns_none() {
        assert!(try_parse_head(b"GET / HTTP/1.1\r\nHost: x\r\n").unwrap().is_none());
    }

    #[test]
    fn parses_method_target_version_and_headers() {
        let head = try_parse_head(b"GET /foo?bar=1 HTTP/1.1\r\nHost: x\r\nCookie: a=1; b=2\r\n\r\nBODY").unwrap().unwrap();
        assert_eq!(head.method, "GET");
        assert_eq!(head.target, "/foo?bar=1");
        assert_eq!(head.version, "HTTP/1.1");
        assert_eq!(head.header("host"), Some("x"));
        assert_eq!(head.header("Cookie"), Some("a=1; b=2"));
        assert_eq!(head.head_bytes_len, b"GET /foo?bar=1 HTTP/1.1\r\nHost: x\r\nCookie: a=1; b=2\r\n\r\n".len());
    }

    #[test]
    fn malformed_request_line_is_an_error() {
        assert!(try_parse_head(b"garbage\r\n\r\n").is_err());
    }

    #[test]
    fn cookie_parsing_splits_pairs() {
        let pairs = parse_cookie_header("gw_token=abc123; other=xyz");
        assert_eq!(pairs, vec![("gw_token".to_string(), "abc123".to_string()), ("other".to_string(), "xyz".to_string())]);
    }

    #[test]
    fn rebuild_strips_only_the_named_cookie_pair() {
        let head = try_parse_head(b"GET / HTTP/1.1\r\nCookie: gw_token=abc; other=xyz\r\n\r\n").unwrap().unwrap();
        let rebuilt = rebuild_head_stripping_cookie(&head, "gw_token");
        let text = String::from_utf8(rebuilt).unwrap();
        assert!(text.contains("Cookie: other=xyz\r\n"), "{text}");
        assert!(!text.contains("gw_token"), "{text}");
    }

    #[test]
    fn rebuild_drops_the_cookie_header_entirely_if_it_was_the_only_pair() {
        let head = try_parse_head(b"GET / HTTP/1.1\r\nCookie: gw_token=abc\r\n\r\n").unwrap().unwrap();
        let rebuilt = rebuild_head_stripping_cookie(&head, "gw_token");
        let text = String::from_utf8(rebuilt).unwrap();
        assert!(!text.to_lowercase().contains("cookie:"), "{text}");
    }

    #[test]
    fn rebuild_leaves_every_other_header_untouched() {
        let head = try_parse_head(b"POST /x HTTP/1.1\r\nHost: y\r\nContent-Type: application/json\r\n\r\n").unwrap().unwrap();
        let rebuilt = rebuild_head_stripping_cookie(&head, "gw_token");
        let text = String::from_utf8(rebuilt).unwrap();
        assert!(text.contains("Host: y\r\n"));
        assert!(text.contains("Content-Type: application/json\r\n"));
    }

    #[test]
    fn build_response_sets_content_length_and_body() {
        let response = build_response(403, "Forbidden", &[("Content-Type", "text/plain".to_string())], b"nope");
        let text = String::from_utf8(response).unwrap();
        assert!(text.starts_with("HTTP/1.1 403 Forbidden\r\n"));
        assert!(text.contains("Content-Length: 4\r\n"));
        assert!(text.ends_with("nope"));
    }

    #[test]
    fn cookie_token_matches_only_the_named_pair_with_the_exact_value() {
        let head = try_parse_head(b"GET / HTTP/1.1\r\nCookie: gw=right; other=x\r\n\r\n").unwrap().unwrap();
        assert!(cookie_token_matches(&head, "gw", "right"));
        assert!(!cookie_token_matches(&head, "gw", "wrong"));
        assert!(!cookie_token_matches(&head, "missing", "right"));
    }

    #[test]
    fn cookie_token_matches_is_false_with_no_cookie_header_at_all() {
        let head = try_parse_head(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").unwrap().unwrap();
        assert!(!cookie_token_matches(&head, "gw", "right"));
    }

    #[test]
    fn random_token_is_64_lowercase_hex_chars_and_varies() {
        let a = random_token();
        let b = random_token();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_ne!(a, b, "two calls should not collide");
    }

    #[test]
    fn hex_encode_matches_known_bytes() {
        assert_eq!(hex_encode(&[0x00, 0xab, 0xff]), "00abff");
    }

    #[tokio::test]
    async fn must_shutdown_returns_once_the_flag_is_set() {
        let (tx, mut rx) = watch::channel(false);
        let waiter = tokio::spawn(async move {
            must_shutdown(&mut rx).await;
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiter.is_finished(), "must_shutdown should not return before the flag is set");
        tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(1), waiter).await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn must_shutdown_returns_if_the_sender_is_dropped() {
        let (tx, mut rx) = watch::channel(false);
        drop(tx);
        tokio::time::timeout(Duration::from_secs(1), must_shutdown(&mut rx)).await.unwrap();
    }

    #[tokio::test]
    async fn read_head_with_cap_returns_the_head_once_fully_buffered() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut client, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            read_head_with_cap(&mut client, &mut buf, 4096, Duration::from_secs(2)).await
        });
        let mut sender = TcpStream::connect(addr).await.unwrap();
        sender.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").await.unwrap();
        let head = server.await.unwrap().unwrap().unwrap();
        assert_eq!(head.method, "GET");
    }

    #[tokio::test]
    async fn read_head_with_cap_rejects_a_head_that_exceeds_the_cap() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut client, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            read_head_with_cap(&mut client, &mut buf, 16, Duration::from_secs(2)).await
        });
        let mut sender = TcpStream::connect(addr).await.unwrap();
        sender.write_all(format!("GET /{} HTTP/1.1\r\n\r\n", "a".repeat(200)).as_bytes()).await.unwrap();
        assert!(server.await.unwrap().unwrap().is_none());
    }

    #[tokio::test]
    async fn read_head_with_cap_errors_if_the_client_closes_before_a_full_head_arrives() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut client, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            read_head_with_cap(&mut client, &mut buf, 4096, Duration::from_secs(2)).await
        });
        let mut sender = TcpStream::connect(addr).await.unwrap();
        sender.write_all(b"GET / HTTP/1.1\r\nHost:").await.unwrap();
        sender.shutdown().await.unwrap();
        drop(sender);
        assert!(server.await.unwrap().is_err(), "an EOF before a full head arrived should be an error, not a silent None");
    }

    #[tokio::test]
    async fn running_loopback_gateway_stop_awaits_its_accept_task() {
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let ran_to_completion = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&ran_to_completion);
        let accept_task = tokio::spawn(async move {
            must_shutdown(&mut shutdown_rx).await;
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let gateway = RunningLoopbackGateway::new(4242, "tok".to_string(), shutdown_tx, accept_task);
        assert_eq!(gateway.port, 4242);
        assert_eq!(gateway.token, "tok");
        gateway.stop().await;
        assert!(ran_to_completion.load(std::sync::atomic::Ordering::SeqCst));
    }
}
