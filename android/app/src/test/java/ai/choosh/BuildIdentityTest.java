package ai.choosh;

import static org.junit.Assert.assertEquals;
import org.junit.Test;

public final class BuildIdentityTest {
    @Test public void packageIdentityIsStable() { assertEquals("ai.choosh", BuildIdentity.packageName()); }
}
