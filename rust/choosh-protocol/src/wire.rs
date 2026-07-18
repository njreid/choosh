//! Typed JSON boundary for handshake messages and version-one envelopes.

use serde_json::{Map, Value};

use crate::envelope::{EnvelopeError, EnvelopeId, Event, Method, Request, Response, Terminal};
use crate::handshake::{
    Capability, HandshakeError, Hello, Incompatible, ProtocolVersion, ServerReply, Welcome,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WireError {
    InvalidLimit,
    PayloadTooLarge,
    MalformedJson,
    InvalidShape,
    UnknownKind,
    Handshake(HandshakeError),
    Envelope(EnvelopeError),
}

impl WireError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidLimit => "invalid_limit",
            Self::PayloadTooLarge => "payload_too_large",
            Self::MalformedJson => "malformed_json",
            Self::InvalidShape => "invalid_shape",
            Self::UnknownKind => "unknown_kind",
            Self::Handshake(error) => error.code(),
            Self::Envelope(error) => error.code(),
        }
    }
}

impl From<HandshakeError> for WireError {
    fn from(error: HandshakeError) -> Self {
        Self::Handshake(error)
    }
}

impl From<EnvelopeError> for WireError {
    fn from(error: EnvelopeError) -> Self {
        Self::Envelope(error)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum WireEnvelope {
    Request(Request<Value>),
    Response(Response<Value, Value>),
    Event(Event<Value>),
}

/// Encodes one bounded client hello with stable object-key/capability order.
///
/// # Errors
///
/// Rejects zero limits or encoded output above the caller's byte ceiling.
pub fn encode_hello(hello: &Hello, max_bytes: usize) -> Result<Vec<u8>, WireError> {
    let capabilities: Vec<_> = hello.capabilities().map(capability_name).collect();
    encode_value(
        &serde_json::json!({
            "kind": "hello",
            "protocol": version_value(hello.protocol),
            "client": identity_value(&hello.client),
            "capabilities": capabilities,
        }),
        max_bytes,
    )
}

/// Decodes one bounded hello object through existing domain constructors.
///
/// # Errors
///
/// Rejects zero limits, oversized/malformed JSON, unknown/additional fields,
/// wrong kinds/types, unknown capabilities, and existing handshake bounds.
pub fn decode_hello(payload: &[u8], max_bytes: usize) -> Result<Hello, WireError> {
    let value = decode_value(payload, max_bytes)?;
    let object = exact_object(&value, &["kind", "protocol", "client", "capabilities"])?;
    if string(object, "kind")? != "hello" {
        return Err(WireError::UnknownKind);
    }
    let protocol = protocol(object.get("protocol").ok_or(WireError::InvalidShape)?)?;
    let client = identity(object.get("client").ok_or(WireError::InvalidShape)?)?;
    let capabilities = object
        .get("capabilities")
        .and_then(Value::as_array)
        .ok_or(WireError::InvalidShape)?
        .iter()
        .map(capability)
        .collect::<Result<Vec<_>, _>>()?;
    Hello::new(protocol, client, capabilities).map_err(Into::into)
}

/// Encodes a negotiated server reply with stable object-key/capability order.
///
/// # Errors
///
/// Rejects zero limits or encoded output above the caller's byte ceiling.
pub fn encode_server_reply(reply: &ServerReply, max_bytes: usize) -> Result<Vec<u8>, WireError> {
    let value = match reply {
        ServerReply::Welcome(welcome) => welcome_value(welcome),
        ServerReply::Incompatible(incompatible) => incompatible_value(*incompatible),
    };
    encode_value(&value, max_bytes)
}

/// Decodes one bounded welcome or incompatible response through existing
/// domain constructors and exact-shape validation.
///
/// # Errors
///
/// Rejects zero limits, oversized/malformed JSON, unknown/additional fields,
/// wrong kinds/types, unknown capabilities, and invalid handshake fields.
pub fn decode_server_reply(payload: &[u8], max_bytes: usize) -> Result<ServerReply, WireError> {
    let value = decode_value(payload, max_bytes)?;
    let object = value.as_object().ok_or(WireError::InvalidShape)?;
    match string(object, "kind")? {
        "welcome" => {
            require_keys(
                object,
                &[
                    "kind",
                    "protocol",
                    "daemon",
                    "host",
                    "capabilities",
                    "limits",
                ],
            )?;
            let limits = exact_object(
                object.get("limits").ok_or(WireError::InvalidShape)?,
                &["max_control_frame_bytes", "max_in_flight_requests"],
            )?;
            let capabilities = object
                .get("capabilities")
                .and_then(Value::as_array)
                .ok_or(WireError::InvalidShape)?
                .iter()
                .map(capability)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ServerReply::Welcome(Welcome::new(
                protocol(object.get("protocol").ok_or(WireError::InvalidShape)?)?,
                identity(object.get("daemon").ok_or(WireError::InvalidShape)?)?,
                identity(object.get("host").ok_or(WireError::InvalidShape)?)?,
                capabilities,
                crate::handshake::ProtocolLimits::new(
                    u32_value(limits, "max_control_frame_bytes")?,
                    u16_value(limits, "max_in_flight_requests")?,
                )?,
            )?))
        }
        "incompatible" => {
            require_keys(object, &["kind", "client", "daemon"])?;
            Ok(ServerReply::Incompatible(Incompatible {
                client: protocol(object.get("client").ok_or(WireError::InvalidShape)?)?,
                daemon: protocol(object.get("daemon").ok_or(WireError::InvalidShape)?)?,
            }))
        }
        _ => Err(WireError::UnknownKind),
    }
}

/// Decodes one bounded post-handshake envelope with untrusted bodies retained as
/// JSON values behind validated IDs, methods, sequence, and terminal shape.
///
/// # Errors
///
/// Rejects zero limits, oversized/malformed JSON, unknown/additional fields,
/// invalid domain identifiers/methods/sequences, or ambiguous response terminals.
pub fn decode_envelope(payload: &[u8], max_bytes: usize) -> Result<WireEnvelope, WireError> {
    let value = decode_value(payload, max_bytes)?;
    let object = value.as_object().ok_or(WireError::InvalidShape)?;
    match string(object, "kind")? {
        "request" => {
            require_keys(object, &["kind", "id", "method", "params"])?;
            Ok(WireEnvelope::Request(Request {
                id: EnvelopeId::new(string(object, "id")?)?,
                method: Method::new(string(object, "method")?)?,
                params: object
                    .get("params")
                    .cloned()
                    .ok_or(WireError::InvalidShape)?,
            }))
        }
        "response" => decode_response(object),
        "event" => {
            require_keys(
                object,
                &["kind", "workspace_id", "sequence", "event", "payload"],
            )?;
            let sequence = object
                .get("sequence")
                .and_then(Value::as_u64)
                .filter(|sequence| *sequence > 0)
                .ok_or(EnvelopeError::InvalidSequence)?;
            Ok(WireEnvelope::Event(Event {
                workspace_id: EnvelopeId::new(string(object, "workspace_id")?)?,
                sequence,
                event: Method::new(string(object, "event")?)?,
                payload: object
                    .get("payload")
                    .cloned()
                    .ok_or(WireError::InvalidShape)?,
            }))
        }
        _ => Err(WireError::UnknownKind),
    }
}

/// Encodes one bounded terminal response with a stable envelope shape.
///
/// The terminal variant selects exactly one of `result` or `error`; caller
/// payloads remain untrusted JSON values and cannot add or replace envelope
/// fields.
///
/// # Errors
///
/// Rejects zero limits or encoded output above the caller's byte ceiling.
pub fn encode_response(
    response: &Response<Value, Value>,
    max_bytes: usize,
) -> Result<Vec<u8>, WireError> {
    let value = match &response.terminal {
        Terminal::Result(result) => serde_json::json!({
            "kind": "response",
            "id": response.id.as_str(),
            "result": result,
        }),
        Terminal::Error(error) => serde_json::json!({
            "kind": "response",
            "id": response.id.as_str(),
            "error": error,
        }),
    };
    encode_value(&value, max_bytes)
}

fn decode_response(object: &Map<String, Value>) -> Result<WireEnvelope, WireError> {
    let has_result = object.contains_key("result");
    let has_error = object.contains_key("error");
    if has_result == has_error {
        return Err(WireError::InvalidShape);
    }
    let terminal = if has_result {
        require_keys(object, &["kind", "id", "result"])?;
        Terminal::Result(object["result"].clone())
    } else {
        require_keys(object, &["kind", "id", "error"])?;
        Terminal::Error(object["error"].clone())
    };
    Ok(WireEnvelope::Response(Response {
        id: EnvelopeId::new(string(object, "id")?)?,
        terminal,
    }))
}

fn decode_value(payload: &[u8], max_bytes: usize) -> Result<Value, WireError> {
    if max_bytes == 0 {
        return Err(WireError::InvalidLimit);
    }
    if payload.len() > max_bytes {
        return Err(WireError::PayloadTooLarge);
    }
    serde_json::from_slice(payload).map_err(|_| WireError::MalformedJson)
}

fn encode_value(value: &Value, max_bytes: usize) -> Result<Vec<u8>, WireError> {
    if max_bytes == 0 {
        return Err(WireError::InvalidLimit);
    }
    let encoded = serde_json::to_vec(value).map_err(|_| WireError::MalformedJson)?;
    if encoded.len() > max_bytes {
        Err(WireError::PayloadTooLarge)
    } else {
        Ok(encoded)
    }
}

fn exact_object<'a>(value: &'a Value, keys: &[&str]) -> Result<&'a Map<String, Value>, WireError> {
    let object = value.as_object().ok_or(WireError::InvalidShape)?;
    require_keys(object, keys)?;
    Ok(object)
}

fn require_keys(object: &Map<String, Value>, keys: &[&str]) -> Result<(), WireError> {
    if object.len() == keys.len() && keys.iter().all(|key| object.contains_key(*key)) {
        Ok(())
    } else {
        Err(WireError::InvalidShape)
    }
}

fn string<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str, WireError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or(WireError::InvalidShape)
}

fn protocol(value: &Value) -> Result<ProtocolVersion, WireError> {
    let object = exact_object(value, &["major", "minor"])?;
    Ok(ProtocolVersion::new(
        u16_value(object, "major")?,
        u16_value(object, "minor")?,
    ))
}

fn identity(value: &Value) -> Result<crate::handshake::PeerIdentity, WireError> {
    let object = exact_object(value, &["name", "version"])?;
    crate::handshake::PeerIdentity::new(string(object, "name")?, string(object, "version")?)
        .map_err(Into::into)
}

fn u16_value(object: &Map<String, Value>, key: &str) -> Result<u16, WireError> {
    let value = object
        .get(key)
        .and_then(Value::as_u64)
        .ok_or(WireError::InvalidShape)?;
    u16::try_from(value).map_err(|_| WireError::InvalidShape)
}

fn u32_value(object: &Map<String, Value>, key: &str) -> Result<u32, WireError> {
    let value = object
        .get(key)
        .and_then(Value::as_u64)
        .ok_or(WireError::InvalidShape)?;
    u32::try_from(value).map_err(|_| WireError::InvalidShape)
}

fn capability(value: &Value) -> Result<Capability, WireError> {
    match value.as_str() {
        Some("events") => Ok(Capability::Events),
        Some("git-blobs") => Ok(Capability::GitBlobs),
        Some("services") => Ok(Capability::Services),
        _ => Err(WireError::InvalidShape),
    }
}

fn capability_name(capability: Capability) -> &'static str {
    match capability {
        Capability::Events => "events",
        Capability::GitBlobs => "git-blobs",
        Capability::Services => "services",
    }
}

fn version_value(version: ProtocolVersion) -> Value {
    serde_json::json!({"major": version.major, "minor": version.minor})
}

fn identity_value(identity: &crate::handshake::PeerIdentity) -> Value {
    serde_json::json!({"name": identity.name, "version": identity.version})
}

fn welcome_value(welcome: &Welcome) -> Value {
    let capabilities: Vec<_> = welcome.capabilities().map(capability_name).collect();
    serde_json::json!({
        "kind": "welcome",
        "protocol": version_value(welcome.protocol),
        "daemon": identity_value(&welcome.daemon),
        "host": identity_value(&welcome.host),
        "capabilities": capabilities,
        "limits": {
            "max_control_frame_bytes": welcome.limits.max_control_frame_bytes,
            "max_in_flight_requests": welcome.limits.max_in_flight_requests
        }
    })
}

fn incompatible_value(incompatible: Incompatible) -> Value {
    serde_json::json!({
        "kind": "incompatible",
        "client": version_value(incompatible.client),
        "daemon": version_value(incompatible.daemon)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handshake::{PeerIdentity, ProtocolLimits, ServerNegotiator};

    const ID: &str = "00000000-0000-0000-0000-000000000001";

    #[test]
    fn hello_round_trips_to_canonical_welcome() {
        let hello = decode_hello(
            br#"{"kind":"hello","protocol":{"major":1,"minor":2},"client":{"name":"android","version":"1"},"capabilities":["services","events"]}"#,
            1024,
        )
        .unwrap();
        let mut server = ServerNegotiator::new(
            ProtocolVersion::new(1, 1),
            PeerIdentity::new("daemon", "1").unwrap(),
            PeerIdentity::new("host", "1").unwrap(),
            [Capability::Events, Capability::Services],
            ProtocolLimits::new(1024, 4).unwrap(),
        )
        .unwrap();
        let reply = server.receive_hello(&hello).unwrap();
        let encoded = encode_server_reply(&reply, 1024).unwrap();
        assert_eq!(
            String::from_utf8(encoded).unwrap(),
            r#"{"capabilities":["events","services"],"daemon":{"name":"daemon","version":"1"},"host":{"name":"host","version":"1"},"kind":"welcome","limits":{"max_control_frame_bytes":1024,"max_in_flight_requests":4},"protocol":{"major":1,"minor":1}}"#
        );
    }

    #[test]
    fn envelopes_validate_ids_methods_sequences_and_terminal_exclusivity() {
        let request = format!(
            r#"{{"kind":"request","id":"{ID}","method":"host.describe","params":{{"x":1}}}}"#
        );
        assert!(matches!(
            decode_envelope(request.as_bytes(), 1024),
            Ok(WireEnvelope::Request(_))
        ));
        let both = format!(r#"{{"kind":"response","id":"{ID}","result":{{}},"error":{{}}}}"#);
        assert_eq!(
            decode_envelope(both.as_bytes(), 1024),
            Err(WireError::InvalidShape)
        );
        let zero = format!(
            r#"{{"kind":"event","workspace_id":"{ID}","sequence":0,"event":"agent.busy","payload":{{}}}}"#
        );
        assert_eq!(
            decode_envelope(zero.as_bytes(), 1024),
            Err(WireError::Envelope(EnvelopeError::InvalidSequence))
        );
    }

    #[test]
    fn response_terminals_encode_canonically_and_round_trip() {
        let id = EnvelopeId::new(ID).unwrap();
        let result = Response {
            id: id.clone(),
            terminal: Terminal::Result(serde_json::json!({"z": 2, "a": 1})),
        };
        let encoded = encode_response(&result, 1024).unwrap();
        assert_eq!(
            String::from_utf8(encoded.clone()).unwrap(),
            format!(r#"{{"id":"{ID}","kind":"response","result":{{"a":1,"z":2}}}}"#)
        );
        assert_eq!(
            decode_envelope(&encoded, 1024),
            Ok(WireEnvelope::Response(result))
        );

        let error = Response {
            id,
            terminal: Terminal::Error(serde_json::json!({"message": "denied"})),
        };
        let encoded = encode_response(&error, 1024).unwrap();
        assert_eq!(
            String::from_utf8(encoded.clone()).unwrap(),
            format!(r#"{{"error":{{"message":"denied"}},"id":"{ID}","kind":"response"}}"#)
        );
        assert_eq!(
            decode_envelope(&encoded, 1024),
            Ok(WireEnvelope::Response(error))
        );
    }

    #[test]
    fn response_encoding_enforces_zero_and_exact_byte_limits() {
        let response = Response {
            id: EnvelopeId::new(ID).unwrap(),
            terminal: Terminal::Result(Value::Null),
        };
        let encoded = encode_response(&response, usize::MAX).unwrap();
        assert_eq!(encode_response(&response, 0), Err(WireError::InvalidLimit));
        assert_eq!(
            encode_response(&response, encoded.len() - 1),
            Err(WireError::PayloadTooLarge)
        );
        assert_eq!(encode_response(&response, encoded.len()), Ok(encoded));
    }

    #[test]
    fn malformed_unknown_extra_and_oversized_inputs_fail_without_echo() {
        assert_eq!(decode_hello(b"{", 16), Err(WireError::MalformedJson));
        assert_eq!(
            decode_envelope(br#"{"kind":"mystery"}"#, 64),
            Err(WireError::UnknownKind)
        );
        assert_eq!(
            decode_hello(br#"{"kind":"hello","extra":true}"#, 64),
            Err(WireError::InvalidShape)
        );
        assert_eq!(decode_envelope(br"{}", 1), Err(WireError::PayloadTooLarge));
    }
}
