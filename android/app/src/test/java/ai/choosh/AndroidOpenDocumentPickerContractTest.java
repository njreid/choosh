package ai.choosh;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

/** Headless policy proof for the picker request, independent of Android Activity Result wiring. */
public final class AndroidOpenDocumentPickerContractTest {
    @Test public void pickerRequestIsOpenableReadOnlyAndDoesNotPersistAccess() {
        AndroidOpenDocumentPicker.RequestPolicy policy = AndroidOpenDocumentPicker.requestPolicy();

        assertEquals("android.intent.action.OPEN_DOCUMENT", policy.action());
        assertTrue(policy.openableOnly());
        assertTrue(policy.readGrant());
        assertFalse(policy.writeGrant());
        assertFalse(policy.persistableGrant());
    }
}
