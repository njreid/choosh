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

/// Lowercase-hex-encodes `bytes` — shared by `web_gateway`/`markdown_gateway`'s
/// per-instance random token generation.
#[must_use]
pub fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
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
    use super::{build_response, parse_cookie_header, rebuild_head_stripping_cookie, try_parse_head};

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
}
