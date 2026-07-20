package ai.choosh;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;

import java.nio.charset.StandardCharsets;
import org.junit.Test;

public final class GitStatusRpcTest {
    private static final String WORKSPACE = "00000000-0000-4000-8000-000000000001";
    private static final String REQUEST = "00000000-0000-4000-8000-000000000002";

    @Test public void encodesOnlyOpaqueWorkspaceIdentity() {
        GitStatusRpc.Request request = request();
        assertEquals(
            "{\"id\":\"" + REQUEST + "\",\"kind\":\"request\",\"method\":\"git.status\",\"params\":{\"workspace_id\":\"" + WORKSPACE + "\"}}",
            new String(request.rpcRequest().copyBytesForNativeAdapter(), StandardCharsets.UTF_8)
        );
    }

    @Test public void decodesCanonicalBytePreservingStatusEntries() throws Exception {
        GitStatusRpc.Result result = GitStatusRpc.decode(request(), response(
            "{\"id\":\"" + REQUEST + "\",\"kind\":\"response\",\"result\":{\"workspace_id\":\"" + WORKSPACE + "\",\"entries\":[{\"staged\":\"renamed\",\"unstaged\":\"unmodified\",\"new_path_b64\":\"bmV3L_8\",\"old_path_b64\":\"b2xkL_4\"}]}}"
        ));
        assertTrue(result.isSuccess());
        assertEquals(1, result.snapshot().entries().size());
        GitStatusRpc.Entry entry = result.snapshot().entries().get(0);
        assertEquals("renamed", entry.staged());
        assertArrayEquals(new byte[] {'n', 'e', 'w', '/', -1}, entry.copyNewPathBytes());
        assertArrayEquals(new byte[] {'o', 'l', 'd', '/', -2}, entry.copyOldPathBytes());
    }

    @Test public void mapsTypedDaemonErrorsWithoutAcceptingTheirMessage() throws Exception {
        GitStatusRpc.Result result = GitStatusRpc.decode(request(), response(
            "{\"id\":\"" + REQUEST + "\",\"kind\":\"response\",\"error\":{\"code\":\"limit_exceeded\",\"message\":\"untrusted text\"}}"
        ));
        assertFalse(result.isSuccess());
        assertEquals(GitStatusRpc.ErrorCode.LIMIT_EXCEEDED, result.error());
        assertNull(result.snapshot());
    }

    @Test public void rejects_id_mismatch_noncanonical_base64_unknown_fields_and_duplicate_json_keys() {
        String[] invalid = {
            "{\"id\":\"00000000-0000-4000-8000-000000000003\",\"kind\":\"response\",\"result\":{\"workspace_id\":\"" + WORKSPACE + "\",\"entries\":[]}}",
            "{\"id\":\"" + REQUEST + "\",\"kind\":\"response\",\"result\":{\"workspace_id\":\"" + WORKSPACE + "\",\"entries\":[{\"staged\":\"modified\",\"unstaged\":\"unmodified\",\"new_path_b64\":\"YQ==\"}]}}",
            "{\"id\":\"" + REQUEST + "\",\"kind\":\"response\",\"result\":{\"workspace_id\":\"" + WORKSPACE + "\",\"entries\":[],\"path\":\"/host/path\"}}",
            "{\"id\":\"" + REQUEST + "\",\"id\":\"" + REQUEST + "\",\"kind\":\"response\",\"result\":{\"workspace_id\":\"" + WORKSPACE + "\",\"entries\":[]}}"
        };
        for (String value : invalid) {
            try {
                GitStatusRpc.decode(request(), response(value));
                throw new AssertionError("malformed result accepted");
            } catch (GitStatusRpc.ProtocolException expected) { }
        }
    }

    private static GitStatusRpc.Request request() {
        return GitStatusRpc.request(new GitStatusRpc.WorkspaceId(WORKSPACE), new GitStatusRpc.RequestId(REQUEST));
    }
    private static AuthenticatedSshOperationCoordinator.RpcResult response(String value) {
        return new AuthenticatedSshOperationCoordinator.RpcResult(value.getBytes(StandardCharsets.UTF_8));
    }
}
