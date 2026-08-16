package ai.choosh

import ai.choosh.engine.AgentEvent
import ai.choosh.engine.MobileProfile
import ai.choosh.engine.ResourceConfirmResult
import ai.choosh.engine.ResourcePattern
import ai.choosh.engine.ResourceProposeResult
import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * [decodeResourceProposeResult]/[decodeResourceConfirmResult]/[decodeWireAgentEvent]
 * against real wire shapes `rust/choosh-android-bridge/src/engine.rs`'s
 * `resource_propose`/`resource_confirm` and `choosh_protocol::relay::WireAgentEvent::ResourceReauthRequired`
 * actually produce — mirrors [NativeChooshEngineEnrollmentTokenDecodeTest]'s
 * "exercise the decode logic directly, no real native `.so` needed" shape.
 */
class NativeChooshEngineResourceDecodeTest {

    // --- resource.propose ---------------------------------------------

    @Test
    fun `decodes a real resource_propose success response`() {
        val result = decodeResourceProposeResult("""{"resource_id":"res-pending-1"}""")
        assertTrue(result is ResourceProposeResult.Success)
        assertEquals("res-pending-1", (result as ResourceProposeResult.Success).resourceId)
    }

    @Test
    fun `decodes the shared error shape into ResourceProposeResult Failure`() {
        val result = decodeResourceProposeResult("""{"error":"invalid pattern: not-a-real-pattern"}""")
        assertTrue(result is ResourceProposeResult.Failure)
        assertEquals("invalid pattern: not-a-real-pattern", (result as ResourceProposeResult.Failure).message)
    }

    // --- resource.confirm -----------------------------------------------

    @Test
    fun `decodes a real resource_confirm approve response with a non-null resource`() {
        val raw = """{"resource":{"resource_id":"res-1","display_name":"Prod AWS SSO","resource_kind":"aws-sso","pattern":"a","mobile_profile":"work","created_by":"operator","last_used_at":null,"last_verified_at":null}}"""
        val result = decodeResourceConfirmResult(raw)
        assertTrue(result is ResourceConfirmResult.Success)
        val resource = (result as ResourceConfirmResult.Success).resource
        assertEquals("res-1", resource?.resourceId)
        assertEquals(ResourcePattern.A, resource?.pattern)
        assertEquals(MobileProfile.WORK, resource?.mobileProfile)
    }

    @Test
    fun `decodes a real resource_confirm reject response with a null resource`() {
        val result = decodeResourceConfirmResult("""{"resource":null}""")
        assertTrue(result is ResourceConfirmResult.Success)
        assertNull((result as ResourceConfirmResult.Success).resource)
    }

    @Test
    fun `decodes a non-reauth resource's null pattern as a null ResourcePattern`() {
        val raw = """{"resource":{"resource_id":"res-2","display_name":"Test host","resource_kind":"custom","pattern":null,"mobile_profile":"ask","created_by":"operator","last_used_at":null,"last_verified_at":null}}"""
        val result = decodeResourceConfirmResult(raw) as ResourceConfirmResult.Success
        assertNull(result.resource?.pattern)
    }

    @Test
    fun `decodes the shared error shape into ResourceConfirmResult Failure`() {
        val result = decodeResourceConfirmResult("""{"error":"not_found: no such pending proposal"}""")
        assertTrue(result is ResourceConfirmResult.Failure)
    }

    // --- WireAgentEvent::ResourceReauthRequired --------------------------

    private fun decodeEventJson(raw: String): AgentEvent = decodeWireAgentEvent(Json.parseToJsonElement(raw))

    @Test
    fun `decodes pattern a with a verification_uri and user_code, no fetch_instructions`() {
        val raw = """{"kind":"resource_reauth_required","resource_id":"res-1","display_name":"Prod AWS SSO","resource_kind":"aws-sso","pattern":"a","verification_uri":"https://example.com/device","user_code":"WDJB-MJHT","fetch_instructions":null,"mobile_profile":"work"}"""
        val event = decodeEventJson(raw)
        assertTrue(event is AgentEvent.ResourceReauthRequired)
        event as AgentEvent.ResourceReauthRequired
        assertEquals("res-1", event.resourceId)
        assertEquals(ResourcePattern.A, event.pattern)
        assertEquals("https://example.com/device", event.verificationUri)
        assertEquals("WDJB-MJHT", event.userCode)
        assertNull(event.fetchInstructions)
        assertEquals(MobileProfile.WORK, event.mobileProfile)
    }

    @Test
    fun `decodes pattern c with fetch_instructions and no verification_uri or user_code`() {
        val raw = """{"kind":"resource_reauth_required","resource_id":"res-2","display_name":"Twilio","resource_kind":"custom","pattern":"c","verification_uri":null,"user_code":null,"fetch_instructions":"Copy your Account SID and Auth Token from https://console.twilio.com/","mobile_profile":"ask"}"""
        val event = decodeEventJson(raw) as AgentEvent.ResourceReauthRequired
        assertNull(event.verificationUri)
        assertNull(event.userCode)
        assertEquals("Copy your Account SID and Auth Token from https://console.twilio.com/", event.fetchInstructions)
        assertEquals(MobileProfile.ASK, event.mobileProfile)
    }

    @Test
    fun `decodes an unrecognized pattern value as ResourcePattern UNKNOWN rather than throwing`() {
        val raw = """{"kind":"resource_reauth_required","resource_id":"res-3","display_name":"x","resource_kind":"custom","pattern":"z","verification_uri":null,"user_code":null,"fetch_instructions":null,"mobile_profile":"ask"}"""
        val event = decodeEventJson(raw) as AgentEvent.ResourceReauthRequired
        assertEquals(ResourcePattern.UNKNOWN, event.pattern)
    }

    @Test
    fun `an unrecognized kind still decodes to Unknown rather than throwing`() {
        val event = decodeEventJson("""{"kind":"something_future"}""")
        assertEquals(AgentEvent.Unknown, event)
    }
}
