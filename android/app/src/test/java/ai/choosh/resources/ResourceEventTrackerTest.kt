package ai.choosh.resources

import ai.choosh.engine.AgentEvent
import ai.choosh.engine.InputReason
import ai.choosh.engine.MobileProfile
import ai.choosh.engine.ResourcePattern
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class ResourceEventTrackerTest {

    private fun reauthEvent(resourceId: String = "res-1", pattern: ResourcePattern = ResourcePattern.A) = AgentEvent.ResourceReauthRequired(
        resourceId = resourceId,
        displayName = "Prod AWS SSO",
        resourceKind = "aws-sso",
        pattern = pattern,
        verificationUri = "https://example.com/device",
        userCode = "WDJB-MJHT",
        fetchInstructions = null,
        mobileProfile = MobileProfile.WORK,
    )

    @Test
    fun `a ResourceReauthRequired push queues a reauth prompt`() {
        val tracker = ResourceEventTracker()
        tracker.apply("dev-1", reauthEvent())

        val prompts = tracker.reauthPrompts.value
        assertEquals(1, prompts.size)
        assertEquals("res-1", prompts[0].resourceId)
        assertEquals("dev-1", prompts[0].fromDeviceId)
        assertEquals("Prod AWS SSO", prompts[0].displayName)
        assertEquals(ResourcePattern.A, prompts[0].pattern)
        assertEquals("https://example.com/device", prompts[0].verificationUri)
        assertEquals("WDJB-MJHT", prompts[0].userCode)
    }

    @Test
    fun `a re-delivered push for the same resource_id replaces, not duplicates, the queued prompt`() {
        val tracker = ResourceEventTracker()
        tracker.apply("dev-1", reauthEvent(pattern = ResourcePattern.A))
        tracker.apply("dev-1", reauthEvent(pattern = ResourcePattern.B))

        val prompts = tracker.reauthPrompts.value
        assertEquals(1, prompts.size)
        assertEquals(ResourcePattern.B, prompts[0].pattern)
    }

    @Test
    fun `two different resource_ids both queue, independently`() {
        val tracker = ResourceEventTracker()
        tracker.apply("dev-1", reauthEvent(resourceId = "res-1"))
        tracker.apply("dev-1", reauthEvent(resourceId = "res-2"))
        assertEquals(setOf("res-1", "res-2"), tracker.reauthPrompts.value.map { it.resourceId }.toSet())
    }

    @Test
    fun `dismissReauthPrompt removes only the named prompt`() {
        val tracker = ResourceEventTracker()
        tracker.apply("dev-1", reauthEvent(resourceId = "res-1"))
        tracker.apply("dev-1", reauthEvent(resourceId = "res-2"))
        tracker.dismissReauthPrompt("res-1")
        assertEquals(listOf("res-2"), tracker.reauthPrompts.value.map { it.resourceId })
    }

    @Test
    fun `an input_required with the RESOURCE_PROPOSAL_WORKSPACE_ID sentinel and Elicitation reason queues a pending proposal`() {
        val tracker = ResourceEventTracker()
        tracker.apply("dev-1", AgentEvent.InputRequired(RESOURCE_PROPOSAL_WORKSPACE_ID, RESOURCE_PROPOSAL_ITEM_ID_PREFIX + "res-pending-1", InputReason.ELICITATION))

        val pending = tracker.pendingProposals.value
        assertEquals(1, pending.size)
        assertEquals("res-pending-1", pending[0].resourceId)
        assertEquals("dev-1", pending[0].fromDeviceId)
    }

    @Test
    fun `a sentinel-workspace input_required whose item_id lacks the resource-proposal prefix never queues a pending proposal`() {
        // Guards RESOURCE_PROPOSAL_ITEM_ID_PREFIX's own reconciliation against
        // hostd's actual format!("resource-proposal:{proposal_id}") shape — a
        // bare id under the sentinel workspace must not be misread as one.
        val tracker = ResourceEventTracker()
        tracker.apply("dev-1", AgentEvent.InputRequired(RESOURCE_PROPOSAL_WORKSPACE_ID, "res-pending-1", InputReason.ELICITATION))
        assertTrue(tracker.pendingProposals.value.isEmpty())
    }

    @Test
    fun `an ordinary workspace-scoped input_required never queues a pending proposal`() {
        val tracker = ResourceEventTracker()
        tracker.apply("dev-1", AgentEvent.InputRequired("ws-1", "item-1", InputReason.QUESTION))
        assertTrue(tracker.pendingProposals.value.isEmpty())
    }

    @Test
    fun `a sentinel input_required with a non-Elicitation reason never queues a pending proposal`() {
        // Per RESOURCE_PROPOSAL_WORKSPACE_ID's own convention, only
        // Elicitation is the resource-proposal shape — a real question
        // (however unlikely to arrive with this workspace_id) must not be
        // misread as one.
        val tracker = ResourceEventTracker()
        tracker.apply("dev-1", AgentEvent.InputRequired(RESOURCE_PROPOSAL_WORKSPACE_ID, RESOURCE_PROPOSAL_ITEM_ID_PREFIX + "res-pending-1", InputReason.APPROVAL))
        assertTrue(tracker.pendingProposals.value.isEmpty())
    }

    @Test
    fun `re-delivering the same pending proposal does not duplicate it`() {
        val tracker = ResourceEventTracker()
        tracker.apply("dev-1", AgentEvent.InputRequired(RESOURCE_PROPOSAL_WORKSPACE_ID, RESOURCE_PROPOSAL_ITEM_ID_PREFIX + "res-pending-1", InputReason.ELICITATION))
        tracker.apply("dev-1", AgentEvent.InputRequired(RESOURCE_PROPOSAL_WORKSPACE_ID, RESOURCE_PROPOSAL_ITEM_ID_PREFIX + "res-pending-1", InputReason.ELICITATION))
        assertEquals(1, tracker.pendingProposals.value.size)
    }

    @Test
    fun `dismissPendingProposal removes only the named proposal`() {
        val tracker = ResourceEventTracker()
        tracker.apply("dev-1", AgentEvent.InputRequired(RESOURCE_PROPOSAL_WORKSPACE_ID, RESOURCE_PROPOSAL_ITEM_ID_PREFIX + "res-pending-1", InputReason.ELICITATION))
        tracker.apply("dev-1", AgentEvent.InputRequired(RESOURCE_PROPOSAL_WORKSPACE_ID, RESOURCE_PROPOSAL_ITEM_ID_PREFIX + "res-pending-2", InputReason.ELICITATION))
        tracker.dismissPendingProposal("res-pending-1")
        assertEquals(listOf("res-pending-2"), tracker.pendingProposals.value.map { it.resourceId })
    }

    @Test
    fun `unrelated event kinds affect neither queue`() {
        val tracker = ResourceEventTracker()
        tracker.apply("dev-1", AgentEvent.TurnCompleted("ws-1", "item-1"))
        tracker.apply("dev-1", AgentEvent.Unknown)
        assertTrue(tracker.reauthPrompts.value.isEmpty())
        assertTrue(tracker.pendingProposals.value.isEmpty())
    }
}
