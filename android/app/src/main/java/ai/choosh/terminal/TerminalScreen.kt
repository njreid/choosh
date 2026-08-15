package ai.choosh.terminal

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
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
            // it reads changes (read via `session.handle` below, even
            // though `attachSession` now takes the whole `session`), so the
            // view gets attached to the real handle as soon as `create()`
            // actually completes.
            update = { view ->
                @Suppress("UNUSED_EXPRESSION") session.handle // read as a Compose State dependency, see comment above
                view.attachSession(session)
            },
            onRelease = { view -> view.detachSession() },
        )

        ExtraKeysBar(session)
    }
}

/**
 * The native Compose extra-keys bar `terminal-experience.md`'s "Extra-keys
 * bar" section requires "immediately above the regular Android keyboard":
 * one-shot Ctrl, Escape/Tab, the arrow cluster, and Home/End/PgUp/PgDn —
 * every button sends a typed key command through [TerminalSession.sendKey]
 * (never a hard-coded escape string), same as the hardware-keyboard path in
 * [TerminalSurfaceView]. Ctrl is one-shot (applies to the next key on this
 * bar, then clears itself), per that section's "one-shot behavior is the V1
 * default". A full `WindowInsets.ime`-following implementation (this bar
 * always stays docked at the bottom of the column, not truly IME-relative)
 * and Alt/Meta modifiers are a further increment this pass didn't reach —
 * this is the required key set, not the full insets/haptics/repeat
 * polish the spec's fuller bullet list also asks for.
 */
@Composable
private fun ExtraKeysBar(session: TerminalSession) {
    var ctrlActive by remember { mutableStateOf(false) }

    fun sendKey(code: Int) {
        session.sendKey(code, ctrl = ctrlActive)
        ctrlActive = false
    }

    Row(
        modifier = Modifier
            .fillMaxWidth()
            .horizontalScroll(rememberScrollState())
            .padding(horizontal = 4.dp, vertical = 4.dp)
            .testTag("terminal-extra-keys-bar"),
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        ExtraKey("Ctrl", "Ctrl modifier, applies to next key", active = ctrlActive, onClick = { ctrlActive = !ctrlActive }, tag = "extra-key-ctrl")
        ExtraKey("Esc", "Escape", onClick = { sendKey(TerminalKeyMapper.KEY_ESCAPE) }, tag = "extra-key-escape")
        ExtraKey("Tab", "Tab", onClick = { sendKey(TerminalKeyMapper.KEY_TAB) }, tag = "extra-key-tab")
        ExtraKey("←", "Left arrow", onClick = { sendKey(TerminalKeyMapper.KEY_LEFT) }, tag = "extra-key-left")
        ExtraKey("↑", "Up arrow", onClick = { sendKey(TerminalKeyMapper.KEY_UP) }, tag = "extra-key-up")
        ExtraKey("↓", "Down arrow", onClick = { sendKey(TerminalKeyMapper.KEY_DOWN) }, tag = "extra-key-down")
        ExtraKey("→", "Right arrow", onClick = { sendKey(TerminalKeyMapper.KEY_RIGHT) }, tag = "extra-key-right")
        ExtraKey("Home", "Home", onClick = { sendKey(TerminalKeyMapper.KEY_HOME) }, tag = "extra-key-home")
        ExtraKey("End", "End", onClick = { sendKey(TerminalKeyMapper.KEY_END) }, tag = "extra-key-end")
        ExtraKey("PgUp", "Page up", onClick = { sendKey(TerminalKeyMapper.KEY_PAGE_UP) }, tag = "extra-key-pgup")
        ExtraKey("PgDn", "Page down", onClick = { sendKey(TerminalKeyMapper.KEY_PAGE_DOWN) }, tag = "extra-key-pgdn")
    }
}

@Composable
private fun ExtraKey(label: String, description: String, active: Boolean = false, onClick: () -> Unit, tag: String) {
    Button(
        onClick = onClick,
        modifier = Modifier
            .defaultMinSize(minWidth = 48.dp, minHeight = 48.dp)
            .testTag(tag)
            .semantics { contentDescription = if (active) "$description, active" else description },
        colors = if (active) {
            ButtonDefaults.buttonColors()
        } else {
            ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.surfaceVariant, contentColor = MaterialTheme.colorScheme.onSurfaceVariant)
        },
        contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 10.dp, vertical = 4.dp),
    ) { Text(label, style = MaterialTheme.typography.labelLarge) }
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
