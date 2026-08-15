//! Wire types and framing for `dev-exec`'s cross-host task offload
//! (`docs/milestones/M7-fleet-and-provisioning.md`'s scope bullet:
//! `"dev-exec --host=<id> <cmd>: cross-host task offload, brokered by
//! relayd, running against a matching jj revision in an ephemeral
//! workspace on the target host"`). M7 is the one milestone with no
//! dedicated spec file, so this module's own doc comments are the
//! normative source for the framing it defines.
//!
//! Carried over an `"offload"`-purpose relay tunnel — see
//! `docs/specs/auth-and-enrollment.md`'s capability table (`devhost`
//! `open-tunnel` to another devhost, `purpose = "offload"`, already
//! enforced relay-side in `choosh-relayd::ws::check_open_tunnel_permitted`)
//! and `docs/specs/relay-protocol.md`'s `open-tunnel`/tunnel-lifecycle
//! sections for the outer envelope this framing rides inside of (an 8-byte
//! tunnel ID, then this module's bytes, per `FRAME_CLASS_TUNNEL`).
//!
//! ## Framing
//!
//! Every tunnel-data payload for an `"offload"` tunnel (i.e. everything
//! after the outer 8-byte tunnel ID relay-protocol.md already strips) opens
//! with one more discriminant byte — this module's own "offload frame
//! kind" — rather than nesting another JSON-tagged envelope for every
//! stdout/stderr chunk, which would base64-bloat the actual command output
//! for no benefit. Only the initial request and an error use JSON;
//! everything else is raw bytes. This mirrors the split already
//! established elsewhere in this codebase: the `rpc`-purpose tunnel uses a
//! `[FRAME_CLASS_CONTROL byte][JSON]` envelope for its (always-structured)
//! request/response traffic, while the `pty:`/`web:` tunnels carry pure
//! bytes with no envelope at all because there's never anything but a byte
//! stream to send. Offload needs both in one tunnel (one structured request,
//! then a byte stream, then a structured result), so it gets the smallest
//! framing that expresses that: a one-byte kind tag.
//!
//! - [`OFFLOAD_FRAME_REQUEST`] (client -> host): JSON [`OffloadRequest`],
//!   sent exactly once, as the tunnel's first data frame.
//! - [`OFFLOAD_FRAME_STDOUT`] (host -> client): raw bytes, any number of
//!   times.
//! - [`OFFLOAD_FRAME_STDERR`] (host -> client): raw bytes, any number of
//!   times.
//! - [`OFFLOAD_FRAME_EXIT`] (host -> client): a 4-byte big-endian signed
//!   exit code, sent exactly once, after every `Stdout`/`Stderr` frame the
//!   command produced. The host proactively sends the ordinary
//!   zero-payload tunnel-close frame (relay-protocol.md) immediately after.
//! - [`OFFLOAD_FRAME_ERROR`] (host -> client): JSON [`OffloadError`], sent
//!   instead of any `Stdout`/`Stderr`/`Exit` frame when the request itself
//!   couldn't be served (an unresolvable workspace/revision, a malformed
//!   request, a command that failed to even start) — followed by the same
//!   proactive tunnel-close.
//!
//! A zero-length tunnel-data payload is never one of this module's frames —
//! that shape is relay-protocol.md's own tunnel-close signal, handled one
//! layer up, before any byte here is interpreted.

use serde::{Deserialize, Serialize};

pub const OFFLOAD_FRAME_REQUEST: u8 = 0x01;
pub const OFFLOAD_FRAME_STDOUT: u8 = 0x02;
pub const OFFLOAD_FRAME_STDERR: u8 = 0x03;
pub const OFFLOAD_FRAME_EXIT: u8 = 0x04;
pub const OFFLOAD_FRAME_ERROR: u8 = 0x05;

/// The originating devhost's offload request, sent once as the tunnel's
/// first data frame.
///
/// `commit_id` is a real Git commit hash — the portable identifier across
/// two independently-cloned `jj` stores of the same underlying Git history.
/// A `jj` *change id* is deliberately NOT used here: it is a local-store
/// concept, assigned independently by each store the first time it imports
/// a commit (per `jj`'s own documented Git-backend behavior), so two hosts'
/// independent clones of the same repo are not guaranteed to agree on a
/// change id for the same commit — only the commit id (a content hash of
/// history both hosts already share) is. See `choosh-hostd::dev_exec`'s
/// module doc comment for the fuller reasoning and the verification this
/// was checked against.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OffloadRequest {
    /// Identifies which of the target devhost's already-registered
    /// Workspaces to run against — the join key across two independent
    /// per-devhost registries, since there is no cross-host Project
    /// identity registry yet (a stated, documented limitation: this
    /// assumes the same conventional workspace name was used to register
    /// the same Project on both hosts).
    pub workspace_name: String,
    pub commit_id: String,
    /// Fixed argv, never a shell string — same "Command construction"
    /// discipline `host-rpc.md` requires of every other `hostd` subprocess
    /// invocation.
    pub argv: Vec<String>,
}

/// A typed, redacted failure to serve an [`OffloadRequest`] — mirrors
/// `host-rpc.md`'s error model (`{ code, message }`, `message` MUST NOT
/// contain file contents, command text, or credentials) even though this
/// isn't itself an RPC response, for the same reasons: this can end up
/// surfaced directly in a terminal or a future UI.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OffloadError {
    pub code: String,
    pub message: String,
}

/// One decoded offload frame — the receiver's-eye view of the framing
/// this module's doc comment documents.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OffloadFrame {
    Request(OffloadRequest),
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    Exit(i32),
    Error(OffloadError),
}

#[derive(Debug)]
pub enum OffloadFrameError {
    /// A tunnel-data payload with no kind byte at all — relay-protocol.md's
    /// own close signal is a zero-length *tunnel* payload one layer up, so
    /// this should never occur for a payload this module is asked to parse
    /// at all; treated as a hard decode error rather than silently ignored.
    Empty,
    UnknownKind(u8),
    Json(serde_json::Error),
    BadExitLength(usize),
}

impl std::fmt::Display for OffloadFrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "offload frame carried no kind byte"),
            Self::UnknownKind(kind) => write!(f, "unrecognized offload frame kind {kind:#04x}"),
            Self::Json(error) => write!(f, "offload frame JSON error: {error}"),
            Self::BadExitLength(len) => write!(f, "offload exit frame carried {len} bytes, expected exactly 4"),
        }
    }
}

impl std::error::Error for OffloadFrameError {}

/// # Errors
///
/// See [`OffloadFrameError`].
pub fn encode_request_frame(request: &OffloadRequest) -> Result<Vec<u8>, serde_json::Error> {
    let mut out = vec![OFFLOAD_FRAME_REQUEST];
    serde_json::to_writer(&mut out, request)?;
    Ok(out)
}

/// # Errors
///
/// See [`OffloadFrameError`].
pub fn encode_error_frame(error: &OffloadError) -> Result<Vec<u8>, serde_json::Error> {
    let mut out = vec![OFFLOAD_FRAME_ERROR];
    serde_json::to_writer(&mut out, error)?;
    Ok(out)
}

#[must_use]
pub fn encode_stdout_frame(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + bytes.len());
    out.push(OFFLOAD_FRAME_STDOUT);
    out.extend_from_slice(bytes);
    out
}

#[must_use]
pub fn encode_stderr_frame(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + bytes.len());
    out.push(OFFLOAD_FRAME_STDERR);
    out.extend_from_slice(bytes);
    out
}

#[must_use]
pub fn encode_exit_frame(code: i32) -> Vec<u8> {
    let mut out = Vec::with_capacity(5);
    out.push(OFFLOAD_FRAME_EXIT);
    out.extend_from_slice(&code.to_be_bytes());
    out
}

/// Decodes one offload frame from `payload` — the bytes of a
/// `FRAME_CLASS_TUNNEL` frame with the outer 8-byte tunnel ID already
/// stripped. `payload` MUST NOT be empty (that shape is relay-protocol.md's
/// tunnel-close signal, handled by the caller before this is ever called).
///
/// # Errors
///
/// See [`OffloadFrameError`].
pub fn decode_frame(payload: &[u8]) -> Result<OffloadFrame, OffloadFrameError> {
    let (&kind, rest) = payload.split_first().ok_or(OffloadFrameError::Empty)?;
    match kind {
        OFFLOAD_FRAME_REQUEST => Ok(OffloadFrame::Request(serde_json::from_slice(rest).map_err(OffloadFrameError::Json)?)),
        OFFLOAD_FRAME_STDOUT => Ok(OffloadFrame::Stdout(rest.to_vec())),
        OFFLOAD_FRAME_STDERR => Ok(OffloadFrame::Stderr(rest.to_vec())),
        OFFLOAD_FRAME_EXIT => {
            let bytes: [u8; 4] = rest.try_into().map_err(|_| OffloadFrameError::BadExitLength(rest.len()))?;
            Ok(OffloadFrame::Exit(i32::from_be_bytes(bytes)))
        }
        OFFLOAD_FRAME_ERROR => Ok(OffloadFrame::Error(serde_json::from_slice(rest).map_err(OffloadFrameError::Json)?)),
        other => Err(OffloadFrameError::UnknownKind(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_frame_round_trips() {
        let request = OffloadRequest {
            workspace_name: "app".to_string(),
            commit_id: "abc123".to_string(),
            argv: vec!["cargo".to_string(), "test".to_string()],
        };
        let encoded = encode_request_frame(&request).unwrap();
        assert_eq!(encoded[0], OFFLOAD_FRAME_REQUEST);
        let decoded = decode_frame(&encoded).unwrap();
        assert_eq!(decoded, OffloadFrame::Request(request));
    }

    #[test]
    fn stdout_and_stderr_frames_carry_raw_bytes_unmodified() {
        let stdout = encode_stdout_frame(b"hello\x00world");
        assert_eq!(decode_frame(&stdout).unwrap(), OffloadFrame::Stdout(b"hello\x00world".to_vec()));

        let stderr = encode_stderr_frame(b"oops");
        assert_eq!(decode_frame(&stderr).unwrap(), OffloadFrame::Stderr(b"oops".to_vec()));
    }

    #[test]
    fn exit_frame_round_trips_including_negative_codes() {
        for code in [0, 1, 7, 127, -1, i32::MIN, i32::MAX] {
            let encoded = encode_exit_frame(code);
            assert_eq!(decode_frame(&encoded).unwrap(), OffloadFrame::Exit(code));
        }
    }

    #[test]
    fn error_frame_round_trips() {
        let error = OffloadError { code: "not_found".to_string(), message: "workspace \"app\" is not registered".to_string() };
        let encoded = encode_error_frame(&error).unwrap();
        assert_eq!(decode_frame(&encoded).unwrap(), OffloadFrame::Error(error));
    }

    #[test]
    fn empty_payload_is_a_decode_error_not_a_panic() {
        assert!(matches!(decode_frame(&[]), Err(OffloadFrameError::Empty)));
    }

    #[test]
    fn unknown_kind_byte_is_a_decode_error() {
        assert!(matches!(decode_frame(&[0xEE, 1, 2, 3]), Err(OffloadFrameError::UnknownKind(0xEE))));
    }

    #[test]
    fn malformed_exit_frame_length_is_a_decode_error() {
        assert!(matches!(decode_frame(&[OFFLOAD_FRAME_EXIT, 1, 2]), Err(OffloadFrameError::BadExitLength(2))));
    }
}
