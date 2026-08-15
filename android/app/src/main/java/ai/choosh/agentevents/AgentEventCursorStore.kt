package ai.choosh.agentevents

/**
 * Per-workspace last-acknowledged agent-event sequence, per
 * docs/specs/agent-events.md's "Delivery and replay" section — what a
 * resume request's `afterSequence` and a `snapshot_required` reset both
 * revolve around.
 *
 * **In-memory only, scoped to this process's lifetime** — explicitly NOT
 * persisted to disk across an app restart. A fresh process has no prior
 * cursor for any workspace, which
 * [ai.choosh.engine.ChooshEngine.agentEventsResume]'s `afterSequence = null`
 * case answers correctly (every currently-retained event, never
 * `snapshot_required` — see that method's own doc comment), so losing this
 * store on process death is a correctness non-issue, just a "replay
 * slightly more than strictly necessary" cost bounded by the devhost's own
 * retention window (`choosh-hostd::agent_event_spool`: 500 events or 1
 * hour). Disk persistence across a restart — so a resume starts from
 * exactly where the app left off rather than the devhost's whole retained
 * window — is a reasonable stretch goal explicitly NOT implemented here.
 */
class AgentEventCursorStore {
    private val cursors = mutableMapOf<String, Long>()

    /** `null` means no prior acknowledged sequence for `workspaceId` — e.g. a first-ever subscribe. */
    fun lastAcknowledged(workspaceId: String): Long? = cursors[workspaceId]

    /**
     * Advances `workspaceId`'s cursor to `sequence` iff it's strictly
     * greater than what's already stored — **never regresses**, regardless
     * of whether it's called for a live push or a replayed event, or in
     * what order callers happen to invoke it (though every real caller in
     * this codebase applies events oldest-first, per `agent-events.md`).
     */
    fun acknowledge(workspaceId: String, sequence: Long) {
        val current = cursors[workspaceId]
        if (current == null || sequence > current) {
            cursors[workspaceId] = sequence
        }
    }

    /**
     * Clears `workspaceId`'s cursor entirely. A `snapshot_required`
     * response to a resume request means this store's prior cursor named a
     * sequence the devhost's spool no longer retains at all — the next
     * subscribe for this workspace should start over as "no prior ack"
     * (always answerable, never itself `snapshot_required` — see this
     * class' own doc comment) rather than keep retrying the same
     * now-permanently-stale sequence forever.
     */
    fun reset(workspaceId: String) {
        cursors.remove(workspaceId)
    }
}
