package ai.choosh.connection

import androidx.credentials.exceptions.CreateCredentialCancellationException
import androidx.credentials.exceptions.CreateCredentialCustomException
import androidx.credentials.exceptions.CreateCredentialInterruptedException
import androidx.credentials.exceptions.CreateCredentialNoCreateOptionException
import androidx.credentials.exceptions.CreateCredentialProviderConfigurationException
import androidx.credentials.exceptions.CreateCredentialUnknownException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test

/**
 * UX-friction audit finding #6: tapping "Set up with a passkey" on a device
 * with no credential provider used to show the raw platform string
 * "No create options available." verbatim. [describeCreateCredentialFailure]
 * replaces every [androidx.credentials.exceptions.CreateCredentialException]'s
 * raw [androidx.credentials.exceptions.CreateCredentialException.errorMessage]
 * with an app-authored one — pinned down here per exception type.
 */
class ConnectionScreenMessagesTest {
    @Test
    fun `a missing credential provider gets a specific, honest explanation, not the raw platform string`() {
        val message = describeCreateCredentialFailure(CreateCredentialNoCreateOptionException("No create options available."))

        assertFalse(
            "the raw platform string must not be shown to the user verbatim",
            message.contains("No create options available"),
        )
        assertEquals(
            "No passkey provider is available on this device — this usually means there's no " +
                "credential provider (e.g. Google Play Services) configured here, so a passkey can't " +
                "be created.",
            message,
        )
    }

    @Test
    fun `cancellation gets a plain cancellation message`() {
        assertEquals("Passkey setup was cancelled.", describeCreateCredentialFailure(CreateCredentialCancellationException()))
    }

    @Test
    fun `a provider configuration failure is described as such`() {
        assertEquals(
            "This device's passkey provider isn't configured correctly.",
            describeCreateCredentialFailure(CreateCredentialProviderConfigurationException()),
        )
    }

    @Test
    fun `an interrupted ceremony invites another try`() {
        assertEquals(
            "Passkey setup was interrupted before it could finish. Please try again.",
            describeCreateCredentialFailure(CreateCredentialInterruptedException()),
        )
    }

    @Test
    fun `an unrecognized exception type still gets an honest, non-fabricated message`() {
        val message = describeCreateCredentialFailure(CreateCredentialUnknownException("some opaque platform detail"))

        assertFalse(
            "must not surface the raw platform detail for a type this app doesn't specifically recognize",
            message.contains("opaque platform detail"),
        )
        assertEquals("Passkey setup couldn't be completed on this device.", message)
    }

    @Test
    fun `a custom provider exception type also gets the same honest fallback`() {
        val message = describeCreateCredentialFailure(CreateCredentialCustomException("com.example.SOME_CUSTOM_TYPE", "vendor-specific detail"))

        assertFalse(message.contains("vendor-specific detail"))
        assertEquals("Passkey setup couldn't be completed on this device.", message)
    }
}
