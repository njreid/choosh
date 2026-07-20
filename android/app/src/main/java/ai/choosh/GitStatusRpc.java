package ai.choosh;

import java.nio.charset.StandardCharsets;
import java.nio.ByteBuffer;
import java.nio.charset.CharacterCodingException;
import java.nio.charset.CodingErrorAction;
import java.util.ArrayList;
import java.util.Base64;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Objects;

/**
 * Headless V1 codec for the opaque-workspace {@code git.status} RPC.
 *
 * <p>This is deliberately a narrow protocol boundary: it creates a single canonical request
 * and rejects malformed host bytes before a controller can retain them. Git paths remain bytes
 * until a later document-display policy explicitly chooses how to decode them.</p>
 */
public final class GitStatusRpc {
    private static final int MAX_ENTRIES = 10_000;
    private static final int MAX_PATH_BYTES = 4_096;

    private GitStatusRpc() { }

    public static Request request(WorkspaceId workspaceId, RequestId requestId) {
        Objects.requireNonNull(workspaceId, "workspaceId");
        Objects.requireNonNull(requestId, "requestId");
        String body = "{\"id\":\"" + requestId.value + "\",\"kind\":\"request\",\"method\":\"git.status\",\"params\":{\"workspace_id\":\"" + workspaceId.value + "\"}}";
        return new Request(workspaceId, requestId, new AuthenticatedSshOperationCoordinator.RpcRequest(
            body.getBytes(StandardCharsets.UTF_8)
        ));
    }

    /** Injectable source so retries do not reuse a terminal RPC identity. */
    public interface RequestSource { Request next(); }

    public static Result decode(Request request, AuthenticatedSshOperationCoordinator.RpcResult response)
        throws ProtocolException {
        Objects.requireNonNull(request, "request");
        Objects.requireNonNull(response, "response");
        Object root = new Parser(response.copyBytesForProtocolDecoder()).parse();
        Map<String, Object> envelope = object(root);
        requireExactKeys(envelope, "id", "kind", "result", "error");
        if (!request.requestId.value.equals(string(envelope.get("id")))
            || !"response".equals(string(envelope.get("kind")))) {
            throw new ProtocolException();
        }
        boolean hasResult = envelope.containsKey("result");
        boolean hasError = envelope.containsKey("error");
        if (hasResult == hasError) throw new ProtocolException();
        if (hasError) return Result.failure(error(envelope.get("error")));
        return Result.success(snapshot(request.workspaceId, envelope.get("result")));
    }

    public static final class WorkspaceId {
        private final String value;
        public WorkspaceId(String value) { this.value = canonicalUuid(value); }
        public String value() { return value; }
        @Override public String toString() { return "WorkspaceId(REDACTED)"; }
    }

    /** Request IDs are injected by the composition root, not generated from ambient state here. */
    public static final class RequestId {
        private final String value;
        public RequestId(String value) { this.value = canonicalUuid(value); }
    }

    public static final class Request {
        private final WorkspaceId workspaceId;
        private final RequestId requestId;
        private final AuthenticatedSshOperationCoordinator.RpcRequest rpcRequest;
        private Request(WorkspaceId workspaceId, RequestId requestId,
            AuthenticatedSshOperationCoordinator.RpcRequest rpcRequest) {
            this.workspaceId = workspaceId;
            this.requestId = requestId;
            this.rpcRequest = rpcRequest;
        }
        public AuthenticatedSshOperationCoordinator.RpcRequest rpcRequest() { return rpcRequest; }
    }

    public static final class Result {
        private final Snapshot snapshot;
        private final ErrorCode error;
        private Result(Snapshot snapshot, ErrorCode error) { this.snapshot = snapshot; this.error = error; }
        static Result success(Snapshot snapshot) { return new Result(snapshot, null); }
        static Result failure(ErrorCode error) { return new Result(null, error); }
        public boolean isSuccess() { return snapshot != null; }
        public Snapshot snapshot() { return snapshot; }
        public ErrorCode error() { return error; }
    }

    public enum ErrorCode { NOT_FOUND, LIMIT_EXCEEDED, PROTOCOL_REJECTED }

    public static final class Snapshot {
        private final List<Entry> entries;
        private Snapshot(List<Entry> entries) {
            this.entries = List.copyOf(entries);
        }
        public List<Entry> entries() { return entries; }
    }

    /** A status entry preserves root-relative Git path bytes, not a host display string. */
    public static final class Entry {
        private final String staged;
        private final String unstaged;
        private final byte[] newPath;
        private final byte[] oldPath;
        private Entry(String staged, String unstaged, byte[] newPath, byte[] oldPath) {
            this.staged = staged; this.unstaged = unstaged;
            this.newPath = newPath.clone(); this.oldPath = oldPath == null ? null : oldPath.clone();
        }
        public String staged() { return staged; }
        public String unstaged() { return unstaged; }
        public byte[] copyNewPathBytes() { return newPath.clone(); }
        public byte[] copyOldPathBytes() { return oldPath == null ? null : oldPath.clone(); }
    }

    public static final class ProtocolException extends Exception { public ProtocolException() { super(); } }

    private static Snapshot snapshot(WorkspaceId expectedWorkspace, Object raw) throws ProtocolException {
        Map<String, Object> value = object(raw);
        requireExactKeys(value, "workspace_id", "entries");
        String workspace = string(value.get("workspace_id"));
        if (!expectedWorkspace.value.equals(workspace)) throw new ProtocolException();
        List<Object> rawEntries = array(value.get("entries"));
        if (rawEntries.size() > MAX_ENTRIES) throw new ProtocolException();
        List<Entry> entries = new ArrayList<>();
        for (Object rawEntry : rawEntries) entries.add(entry(rawEntry));
        return new Snapshot(entries);
    }

    private static Entry entry(Object raw) throws ProtocolException {
        Map<String, Object> value = object(raw);
        requireExactKeys(value, "staged", "unstaged", "new_path_b64", "old_path_b64");
        String staged = status(string(value.get("staged")));
        String unstaged = status(string(value.get("unstaged")));
        byte[] newPath = base64Path(string(value.get("new_path_b64")));
        byte[] oldPath = value.containsKey("old_path_b64") ? base64Path(string(value.get("old_path_b64"))) : null;
        return new Entry(staged, unstaged, newPath, oldPath);
    }

    private static ErrorCode error(Object raw) throws ProtocolException {
        Map<String, Object> value = object(raw);
        requireExactKeys(value, "code", "message");
        String code = string(value.get("code"));
        if ("not_found".equals(code)) return ErrorCode.NOT_FOUND;
        if ("limit_exceeded".equals(code)) return ErrorCode.LIMIT_EXCEEDED;
        return ErrorCode.PROTOCOL_REJECTED;
    }

    private static String canonicalUuid(String value) {
        if (value == null || !value.matches("[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")) {
            throw new IllegalArgumentException("invalid opaque identifier");
        }
        return value;
    }
    private static String status(String value) throws ProtocolException {
        if (!("unmodified".equals(value) || "modified".equals(value) || "added".equals(value)
            || "deleted".equals(value) || "renamed".equals(value) || "copied".equals(value)
            || "updated_but_unmerged".equals(value) || "untracked".equals(value)
            || "ignored".equals(value))) throw new ProtocolException();
        return value;
    }
    private static byte[] base64Path(String value) throws ProtocolException {
        if (value.isEmpty() || value.contains("=") || !value.matches("[A-Za-z0-9_-]+")) throw new ProtocolException();
        try {
            byte[] decoded = Base64.getUrlDecoder().decode(value);
            if (decoded.length == 0 || decoded.length > MAX_PATH_BYTES
                || !Base64.getUrlEncoder().withoutPadding().encodeToString(decoded).equals(value)) throw new ProtocolException();
            return decoded;
        } catch (IllegalArgumentException exception) { throw new ProtocolException(); }
    }
    @SuppressWarnings("unchecked") private static Map<String, Object> object(Object value) throws ProtocolException {
        if (!(value instanceof Map)) throw new ProtocolException(); return (Map<String, Object>) value;
    }
    @SuppressWarnings("unchecked") private static List<Object> array(Object value) throws ProtocolException {
        if (!(value instanceof List)) throw new ProtocolException(); return (List<Object>) value;
    }
    private static String string(Object value) throws ProtocolException {
        if (!(value instanceof String)) throw new ProtocolException(); return (String) value;
    }
    private static void requireExactKeys(Map<String, Object> object, String... allowed) throws ProtocolException {
        for (String key : object.keySet()) {
            boolean present = false; for (String candidate : allowed) if (candidate.equals(key)) present = true;
            if (!present) throw new ProtocolException();
        }
    }

    /** Strict bounded JSON parser: duplicate object keys and trailing data are rejected. */
    private static final class Parser {
        private final String source; private int cursor;
        Parser(byte[] input) throws ProtocolException {
            try {
                source = StandardCharsets.UTF_8.newDecoder()
                    .onMalformedInput(CodingErrorAction.REPORT)
                    .onUnmappableCharacter(CodingErrorAction.REPORT)
                    .decode(ByteBuffer.wrap(input)).toString();
            } catch (CharacterCodingException exception) { throw new ProtocolException(); }
        }
        Object parse() throws ProtocolException { Object value = value(); whitespace(); if (cursor != source.length()) throw new ProtocolException(); return value; }
        private Object value() throws ProtocolException { whitespace(); if (cursor >= source.length()) throw new ProtocolException(); char c = source.charAt(cursor); if (c == '{') return objectValue(); if (c == '[') return arrayValue(); if (c == '"') return stringValue(); throw new ProtocolException(); }
        private Map<String, Object> objectValue() throws ProtocolException { cursor++; Map<String, Object> result = new LinkedHashMap<>(); whitespace(); if (take('}')) return result; while (true) { whitespace(); String key = stringValue(); whitespace(); expect(':'); Object value = value(); if (result.put(key, value) != null) throw new ProtocolException(); whitespace(); if (take('}')) return result; expect(','); } }
        private List<Object> arrayValue() throws ProtocolException { cursor++; List<Object> result = new ArrayList<>(); whitespace(); if (take(']')) return result; while (true) { result.add(value()); whitespace(); if (take(']')) return result; expect(','); } }
        private String stringValue() throws ProtocolException { expect('"'); StringBuilder result = new StringBuilder(); while (cursor < source.length()) { char c = source.charAt(cursor++); if (c == '"') return result.toString(); if (c == '\\') { if (cursor >= source.length()) throw new ProtocolException(); char escaped = source.charAt(cursor++); if (escaped == '"' || escaped == '\\' || escaped == '/') result.append(escaped); else if (escaped == 'b') result.append('\b'); else if (escaped == 'f') result.append('\f'); else if (escaped == 'n') result.append('\n'); else if (escaped == 'r') result.append('\r'); else if (escaped == 't') result.append('\t'); else if (escaped == 'u' && cursor + 4 <= source.length()) { try { result.append((char) Integer.parseInt(source.substring(cursor, cursor + 4), 16)); cursor += 4; } catch (NumberFormatException exception) { throw new ProtocolException(); } } else throw new ProtocolException(); } else if (c < 0x20) throw new ProtocolException(); else result.append(c); } throw new ProtocolException(); }
        private void whitespace() { while (cursor < source.length() && Character.isWhitespace(source.charAt(cursor))) cursor++; }
        private boolean take(char expected) { if (cursor < source.length() && source.charAt(cursor) == expected) { cursor++; return true; } return false; }
        private void expect(char expected) throws ProtocolException { if (!take(expected)) throw new ProtocolException(); }
    }
}
