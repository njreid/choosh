package ai.choosh.resources

import ai.choosh.engine.AgentEvent
import ai.choosh.engine.InputReason
import ai.choosh.engine.MobileProfile
import ai.choosh.engine.ResourcePattern
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * `choosh_protocol::relay::WireAgentEvent::InputRequired` carries only
 * `{workspace_id, item_id, reason}` — no `resource_id` field — so an
 * agent's pending-resource-proposal `input_required`/`Elicitation` event
 * (docs/specs/resources-and-reauth.md's "Agent-declared resources":
 * "reusing the same mechanism already built for 'ask a human a question'")
 * has to encode which pending `resource_id` it's about using only those
 * three fields. Reconciled against `rust/choosh-hostd/src/rpc.rs`'s actual
 * `handle_resource_propose` (built concurrently, in a separate work
 * stream): `workspace_id` is this sentinel, `item_id` is
 * `"$RESOURCE_PROPOSAL_ITEM_ID_PREFIX$resourceId"` — NOT the bare
 * `resource_id` this app originally assumed before reconciliation.
 */
const val RESOURCE_PROPOSAL_WORKSPACE_ID = "devhost"

/** See [RESOURCE_PROPOSAL_WORKSPACE_ID]'s doc comment — matches hostd's `format!("resource-proposal:{proposal_id}")` exactly. */
const val RESOURCE_PROPOSAL_ITEM_ID_PREFIX = "resource-proposal:"

/** One agent-proposed Resource awaiting the human's approve/reject, per [RESOURCE_PROPOSAL_WORKSPACE_ID]'s doc comment. */
data class PendingResourceProposal(val resourceId: String, val fromDeviceId: String)

/**
 * One live [AgentEvent.ResourceReauthRequired] push, queued for the phone
 * UI to act on. Mirrors that event's own fields field-for-field — see
 * [ResourcePattern]'s doc comment for which of [verificationUri]/[userCode]/
 * [fetchInstructions] is populated for a given [pattern].
 */
data class ResourceReauthPrompt(
    val resourceId: String,
    val fromDeviceId: String,
    val displayName: String,
    val resourceKind: String,
    val pattern: ResourcePattern,
    val verificationUri: String?,
    val userCode: String?,
    val fetchInstructions: String?,
    val mobileProfile: MobileProfile,
)

/**
 * Tracks resource-related agent events for phone-side UI, mirroring
 * [ai.choosh.agentevents.AgentAttentionTracker]/[ai.choosh.agentevents.AgentStatusTracker]'s
 * existing "apply(event)" shape so [ai.choosh.agentevents.AgentEventSubscription]
 * can feed it identically (replayed or live) alongside those two trackers.
 *
 * Two independent surfaces, both queues (a devhost can raise more than one
 * before the human deals with the first) rather than a single latest-wins
 * slot:
 * - [reauthPrompts]: every live [AgentEvent.ResourceReauthRequired] not yet
 *   dismissed.
 * - [pendingProposals]: every live agent-proposed-Resource `input_required`
 *   not yet resolved, per [RESOURCE_PROPOSAL_WORKSPACE_ID]'s convention.
 *
 * Unlike [ai.choosh.agentevents.AgentAttentionTracker]'s `TurnCompleted`-clears
 * rule, neither surface here has an implicit "the agent resumed" signal —
 * both are cleared explicitly by the UI layer once the human has acted
 * ([dismissReauthPrompt]/[dismissPendingProposal]).
 */
class ResourceEventTracker {
    private val _reauthPrompts = MutableStateFlow<List<ResourceReauthPrompt>>(emptyList())
    val reauthPrompts: StateFlow<List<ResourceReauthPrompt>> = _reauthPrompts.asStateFlow()

    private val _pendingProposals = MutableStateFlow<List<PendingResourceProposal>>(emptyList())
    val pendingProposals: StateFlow<List<PendingResourceProposal>> = _pendingProposals.asStateFlow()

    fun apply(fromDeviceId: String, event: AgentEvent) {
        when {
            event is AgentEvent.ResourceReauthRequired -> {
                val prompt = ResourceReauthPrompt(
                    resourceId = event.resourceId,
                    fromDeviceId = fromDeviceId,
                    displayName = event.displayName,
                    resourceKind = event.resourceKind,
                    pattern = event.pattern,
                    verificationUri = event.verificationUri,
                    userCode = event.userCode,
                    fetchInstructions = event.fetchInstructions,
                    mobileProfile = event.mobileProfile,
                )
                // A re-delivered/replayed push for the same resource_id
                // replaces the earlier one rather than duplicating it — the
                // newest state for a given resource is what the human
                // should act on.
                _reauthPrompts.value = _reauthPrompts.value.filterNot { it.resourceId == event.resourceId } + prompt
            }
            event is AgentEvent.InputRequired &&
                event.reason == InputReason.ELICITATION &&
                event.workspaceId == RESOURCE_PROPOSAL_WORKSPACE_ID &&
                event.itemId.startsWith(RESOURCE_PROPOSAL_ITEM_ID_PREFIX) -> {
                val resourceId = event.itemId.removePrefix(RESOURCE_PROPOSAL_ITEM_ID_PREFIX)
                if (_pendingProposals.value.none { it.resourceId == resourceId }) {
                    _pendingProposals.value = _pendingProposals.value + PendingResourceProposal(resourceId, fromDeviceId)
                }
            }
        }
    }

    /** Removes `resourceId`'s prompt — called once the human has completed, submitted, or explicitly dismissed it. */
    fun dismissReauthPrompt(resourceId: String) {
        _reauthPrompts.value = _reauthPrompts.value.filterNot { it.resourceId == resourceId }
    }

    /** Removes `resourceId`'s pending proposal — called once [ai.choosh.engine.ChooshEngine.resourceConfirm] has resolved it (approve or reject). */
    fun dismissPendingProposal(resourceId: String) {
        _pendingProposals.value = _pendingProposals.value.filterNot { it.resourceId == resourceId }
    }
}
