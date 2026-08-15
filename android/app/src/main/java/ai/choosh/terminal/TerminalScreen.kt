package ai.choosh.terminal

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import java.nio.charset.StandardCharsets

/**
 * The `AgentTerminal` page, per `docs/specs/android-navigation.md`'s pinned
 * kinds table and `docs/specs/terminal-experience.md`. Hosts the native
 * `TerminalSurfaceView` (wgpu/glyphon renderer over an `ANativeWindow`) and
 * owns the native session's lifecycle: created when this composable enters
 * composition, destroyed when it leaves.
 *
 * `deviceId`/`itemId` identify the Zellij pane this terminal attaches to
 * (per terminal-experience.md's `pty:<item_id>` tunnel). `connectionHandle`
 * is the live `NativeBridge` connection handle from
 * [ai.choosh.NativeChooshEngine] — `null` when the app is running against
 * [ai.choosh.engine.FakeChooshEngine] (no real relay connection exists
 * yet), in which case the extra-keys row's "Demo output" button is the
 * only way to see anything render — a real PTY attach needs a real
 * connection, which this composable does not fabricate.
 */
@Composable
fun TerminalScreen(deviceId: String, itemId: String, connectionHandle: Long?, onBack: () -> Unit) {
    val session = remember { TerminalSession() }

    DisposableEffect(deviceId, itemId) {
        session.create(cols = 80, rows = 24)
        if (connectionHandle != null) {
            session.attachPty(connectionHandle, deviceId, itemId)
        }
        onDispose { session.destroy() }
    }

    Column(modifier = Modifier.fillMaxSize()) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(8.dp),
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            Text("Terminal: $itemId@$deviceId", style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
            Button(onClick = onBack) { Text("Back") }
        }

        // A demo/verification affordance: injects a canned, real ANSI byte
        // sequence directly into the VT parser without needing a live PTY
        // tunnel — the same code path the on-device rendering verification
        // in this increment's evidence used. Kept visible (not just a test
        // hook) since it's also a reasonable way to prove the surface is
        // alive before a real relay deployment exists to attach to. Placed
        // above the surface (not below it) so it never competes with the
        // system gesture-nav area at the bottom of the screen.
        Row(modifier = Modifier.fillMaxWidth().padding(8.dp), horizontalArrangement = Arrangement.SpaceEvenly) {
            Button(onClick = { session.testInject(demoPromptAndListing()) }) { Text("Demo output") }
            Button(onClick = { session.testInject(demoFullScreenRedraw()) }) { Text("Full redraw") }
        }

        AndroidView(
            modifier = Modifier.fillMaxWidth().weight(1f),
            factory = { context -> TerminalSurfaceView(context) },
            // `update` (not just `factory`) is required: `session.create()`
            // runs in a `DisposableEffect` whose ordering relative to this
            // `factory` call within the same composition isn't guaranteed,
            // so `session.handle` may still be 0 the first time `factory`
            // runs. `update` re-runs whenever the `handle` Compose `State`
            // it reads changes, so the view gets attached to the real
            // handle as soon as `create()` actually completes.
            update = { view -> view.attachSession(session.handle) },
            onRelease = { view -> view.detachSession() },
        )
    }
}

/** ANSI byte sequence resembling a colored shell prompt plus `ls -la` output. */
private fun demoPromptAndListing(): ByteArray {
    val text = buildString {
        append("\u001b[1;32muser@choosh\u001b[0m:\u001b[1;34m~/project\u001b[0m$ ls -la\r\n")
        append("total 24\r\n")
        append("drwxr-xr-x  5 user user 4096 Aug 14 12:00 \u001b[1;34m.\u001b[0m\r\n")
        append("drwxr-xr-x  3 user user 4096 Aug 14 11:58 \u001b[1;34m..\u001b[0m\r\n")
        append("-rw-r--r--  1 user user  220 Aug 14 11:58 .bashrc\r\n")
        append("-rwxr-xr-x  1 user user 8192 Aug 14 12:00 \u001b[1;32mrun.sh\u001b[0m\r\n")
        append("drwxr-xr-x  8 user user 4096 Aug 14 12:00 \u001b[1;34msrc\u001b[0m\r\n")
        append("\u001b[1;32muser@choosh\u001b[0m:\u001b[1;34m~/project\u001b[0m$ \u001b[?25h")
    }
    return text.toByteArray(StandardCharsets.UTF_8)
}

/** A full-screen clear + redraw, exercising the damage-cache reset path. */
private fun demoFullScreenRedraw(): ByteArray {
    val text = buildString {
        append("\u001b[2J\u001b[H")
        for (row in 0 until 24) {
            append("\u001b[38;5;${(row * 9) % 256}mrow $row: the quick brown fox jumps over the lazy dog\u001b[0m\r\n")
        }
    }
    return text.toByteArray(StandardCharsets.UTF_8)
}
