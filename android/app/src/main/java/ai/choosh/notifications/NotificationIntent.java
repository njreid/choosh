package ai.choosh.notifications;

import java.util.Objects;

/** Redacted, platform-neutral notification intent. */
public record NotificationIntent(
        String hostId,
        String workspaceId,
        String itemId,
        String workspaceName,
        String agentName,
        String reason) {
    public NotificationIntent {
        require(hostId, "hostId"); require(workspaceId, "workspaceId"); require(itemId, "itemId");
        require(workspaceName, "workspaceName"); require(agentName, "agentName"); require(reason, "reason");
    }
    public String key() { return hostId + ":" + workspaceId + ":" + itemId; }
    private static void require(String value, String name) {
        if (value == null || value.isBlank() || value.indexOf('\0') >= 0) throw new IllegalArgumentException(name);
    }
}
