# Device and accessibility testing report (M8)

Hands-on pass against a real Genymotion cloud Android device
(`172.31.0.13:5555`, Android 16, `arm64-v8a`, AOSP-flavored — no Google
Play Services, no Play Store), driving a real debug APK
(`./gradlew :app:assembleDebug`, `adb install`) built with
`CARGO_TARGET_DIR=/home/njr/.cache/choosh-cargo-target`. All screenshots
and `uiautomator` dumps referenced below are saved under
`/tmp/claude-1001/-home-njr-code-choosh/997cb4ee-5174-4bef-b66a-526b7582c38c/scratchpad/`
(file names given per item). Every verdict below is backed by a named
artifact in that directory, not a verbal impression.

## Methodology note: how the app was reached (read this first)

`ChooshApp.kt` starts at `Screen.Connection`, which requires a real
Android Credential Manager WebAuthn passkey ceremony
(`CreatePublicKeyCredentialRequest`) before the Fleet drawer or any
Workspace/pinned-item screen is reachable. On this specific Genymotion
device, Credential Manager has **zero `CredentialProviderService`
implementations registered** — confirmed both with no device lock and
with a PIN configured (`adb shell locksettings set-pin 1234`) — so the
ceremony always terminates in "No create options available."
(`dev09_passkey_pin_retry.png`), independent of any app defect. This is
an environment limitation of this sandboxed device (no Google Play
Services, no password-manager app providing a local passkey), not
something fixable from the app side without weakening the app's
passkey-only auth posture (a bypass button was tried and explicitly
rejected during this pass — see "Bugs found" below).

To still drive the real, unmodified production screens on the real
device for items 1–5, this pass added
`android/app/src/androidTest/java/ai/choosh/DeviceVerificationHarnessTest.kt`,
an `androidTest`-only file (never packaged into `app-debug.apk` or a
release build) that mounts the exact same production composables
(`ExplorerScreen`, `JjDiffScreen`, `JjChangeGraphScreen`, `TerminalScreen`,
`WebServiceScreen`, `MarkdownFixtureDemoScreen`, `FleetDrawer`) against
the same `FakeChooshEngine` fixture data `ChooshApp` would use, following
this repo's own established on-device verification convention (see
`ai.choosh.sourceeditor.SourceEditorScreenshotVerificationTest`'s doc
comment). Every pixel captured below is unmodified production Kotlin
rendered by the real GPU/View pipeline on the real device — this
substitutes only for `ChooshApp`'s Connection→Fleet navigation shell, not
for any screen under test. Tests were driven individually via
`adb shell am instrument -w -e class ai.choosh.DeviceVerificationHarnessTest#<method>
ai.choosh.test/androidx.test.runner.AndroidJUnitRunner`, each holding its
screen for tens of seconds (logging a `DeviceHarness` marker) while an
external `adb` session captured screenshots/dumps/meminfo — the same
technique this repo already uses for `SourceEditorScreenshotVerificationTest`.

TalkBack itself is not installed on this device and could not be
installed (no Play Store; sideloading the APK from third-party mirrors
was not permitted). As a substitute, this pass used
`adb shell uiautomator dump` — which reads the same Android
`AccessibilityNodeInfo` tree TalkBack itself consumes — to inspect labels,
`content-desc`, and `clickable`/`focusable` flags per screen. This is a
reasonable proxy for *what labels/nodes exist* but cannot fully reproduce
TalkBack's own announcement/merging heuristics or its linear swipe-order
UX; findings below are phrased accordingly. One known blind spot: Android
`WebView` only populates its native accessibility node tree when a real
`AccessibilityService` is bound and requesting it, so `uiautomator dump`
under-reports `WebService`/`Markdown` `WebView` content (see item 1).

---

## 1. Screen-reader (TalkBack) pass — **gap found**

**Evidence:**
- Explorer: `a1_explorer.png`, `a1_explorer.xml`
- JjDiff: `a2_diff.png`, `a2_diff.xml`
- JjChangeGraph + detail dialog: `a3_graph.png`/`a3_graph.xml`,
  `a4_graph_dialog.png`/`a4_graph_dialog.xml`
- Terminal (blank and with content): `b1_terminal_blank.png`/`.xml`,
  `b3_terminal_demo_output.png`
- Fleet drawer: `h1_fleet_phone.png`/`h1_fleet_phone.xml`
- WebService (Starting interstitial): `c1_webservice_starting.png`
- Markdown (real WebView content): `d1_markdown.png`/`d1_markdown.xml`

**Gap 1 — every custom `Modifier.clickable()` row across Explorer, JjDiff,
JjChangeGraph, and (by identical code pattern) Fleet exposes an empty
accessible label on the actionable node itself.** Concretely, in every
`uiautomator` dump above, every node with `clickable="true"` has
`text=""` and no `content-desc`; the human-readable label instead lives
on a separate, non-clickable, non-focusable sibling `TextView` (e.g.
`ExplorerScreen.kt`'s `ChangedFileRow`, `JjChangeGraphScreen.kt`'s
`ChangeNodeChip` and `OperationRow`'s "Restore" `TextButton`, even
Material3's own `Button`/`TextButton` composables in this dump). None of
these call `Modifier.semantics(mergeDescendants = true)` or set an
explicit `contentDescription`. By contrast,
`FleetDrawer.kt`'s `SortModeIcon` and `AttentionDot` *do* set
`Modifier.semantics { contentDescription = ... }` explicitly and (by the
same reasoning) would show correctly on their own node — confirming this
is a per-callsite omission, not a framework limitation. Expected: the
clickable row's own node carries a meaningful merged label (e.g.
"README.md, added" or "change-merge, merge A and B"). Actual: the
clickable node is unlabeled; TalkBack would either announce nothing
useful for it or fragment one logical row into 2–3 separate stops.
Action: add `Modifier.semantics(mergeDescendants = true) {}` (or an
explicit `contentDescription`) to every custom clickable container built
from raw `Text` children — `ChangedFileRow`, `ChangeNodeChip`,
`OperationRow`'s Restore button, `FleetRowView`, and the diff's
`OutlinedTextField`-adjacent `Load diff` button pattern.

**Gap 2 — the Terminal surface has zero accessible content.** In
`b1_terminal_blank.xml`, `TerminalSurfaceView` (`android.view.SurfaceView`,
bounds `[0,144][600,1024]` — the full terminal viewport) has no `text`
and no `content-desc`, confirmed by source inspection: it is a bare
`SurfaceView` (`android/app/src/main/java/ai/choosh/terminal/TerminalSurfaceView.kt`)
with no `contentDescription`, no `Modifier.semantics`, not
focusable/clickable. This holds whether the terminal is blank or showing
real rendered content (`b3_terminal_demo_output.png` shows real ANSI
`ls -la` output; the underlying accessibility node is identical, empty).
A screen-reader user gets no indication this region contains a terminal,
let alone its content. `terminal-experience.md` explicitly requires
"Kotlin owns... accessibility semantics" for the terminal host — not yet
implemented. Action: at minimum, give `TerminalSurfaceView` (or its
Compose wrapper in `TerminalScreen.kt`) a `contentDescription` describing
it as an interactive terminal; a fuller fix (live-region announcements of
new output, or an accessible text mirror) is a larger design question
out of scope for a one-line fix.

**Not a gap — Material3-standard elements are fine.** The `AlertDialog`
(`a4_graph_dialog.xml`), `OutlinedTextField` labels
(`a2_diff.xml` — "From (default @-)"/"To (default @)" render as
associated `EditText` labels, a first-party Material3 mechanism not
flagged here), and the WebService "Starting…" interstitial
(`CircularProgressIndicator` + `Text`) all use stock Material3 components
with standard, reviewed accessibility behavior.

**Caveat — not evaluated.** `WebService`/`Markdown` `WebView` page
content (`d1_markdown.xml` shows the `WebView` node itself but not its
DOM content, per the WebView-accessibility-injection blind spot noted
above) was not evaluated for in-page label quality; this needs a live
TalkBack session (not `uiautomator dump`) to check properly.

---

## 2. Hardware keyboard behavior — **gap found (Terminal); inconclusive for Tab/Escape**

**Evidence:**
- Terminal, blank, before/after `adb shell input keyevent KEYCODE_A` +
  `input text hello_from_hw_keyboard` + Ctrl longpress + DPAD_RIGHT:
  `b1_terminal_blank.png` / `b2_terminal_after_hwkeys_blank.png` —
  pixel-identical.
- Terminal, with real rendered content, before/after `KEYCODE_A` +
  `input text SHOULD_NOT_APPEAR` + `KEYCODE_ENTER`:
  `b3_terminal_demo_output.png` / `b4_terminal_after_hwkeys_content.png`
  — pixel-identical (cursor unchanged, no injected text visible).
- `adb shell dumpsys input` at the time of the blank-terminal key test:
  focused window is `ai.choosh/androidx.activity.ComponentActivity`
  (the Activity itself, no specific input-consuming child).
- Source: `grep -rn "sendKey(\|sendText(\|paste(\|sendMouse(" android/app/src/main/java/ai/choosh/`
  returns **zero call sites** anywhere in the app's UI code —
  `TerminalSession.sendKey`/`sendText`/`paste`/`sendMouse`
  (`android/app/src/main/java/ai/choosh/terminal/TerminalSession.kt`)
  are only ever defined, never invoked. `TerminalScreen.kt` wires only
  two demo-output *injection* buttons (`testInject`), nothing that reads
  a real Android `KeyEvent`/`InputConnection`. No `KeyEvent`,
  `onKeyEvent`, `onPreviewKeyEvent`, `InputConnection`, `focusable`, or
  `FocusRequester` usage exists anywhere in `android/app/src/main/java/ai/choosh/`.

**Verdict: confirmed gap.** `docs/specs/terminal-experience.md` requires
"a real Android `InputConnection`" that "must handle... hardware
keyboards" and an extra-keys bar with modifiers/arrows/Home/End/PgUp/PgDn
— none of this exists yet. The two on-screen "Demo output"/"Full redraw"
buttons are the *only* way to put content on the terminal today; a real
hardware keyboard (or the soft keyboard) does nothing. This is
independently confirmed twice on the real device (blank and
content-showing states) and matches the source-level absence of any key
dispatch path. Action: wire `TerminalSurfaceView`/`TerminalScreen` to a
real `InputConnection` (or at minimum `Modifier.onKeyEvent` +
`focusable()`) that calls the already-defined but unused
`TerminalSession.sendKey`/`sendText`, per `terminal-experience.md`'s
"Android IME" and "Extra-keys bar" sections — this is a full increment of
work, not a one-line fix, and is the single most consequential finding
of this pass, since the terminal is the app's core surface.

**Tab-focus-between-fields and Escape/Back-dismisses-dialog: inconclusive,
not a confirmed defect either way.** Two live experiments against the
`androidTest` harness did not behave as expected:
- Tapping the JjDiff screen's "From" field (`g1_diff_from_focused.png`)
  then sending `KEYCODE_TAB` and `input text AFTERTAB` produced no visible
  focus ring and no typed text (`g2_diff_after_tab_and_type.png` is
  unchanged from `g1`).
- Sending `KEYCODE_ESCAPE` (`e2_dialog_after_escape.png`), `KEYCODE_BACK`
  (`e3_dialog_after_back.png`), tapping the on-screen nav-bar back icon
  (`e4_dialog_after_navbar_back_tap.png`), and even tapping the dialog's
  own visible "Close" button at its exact dumped bounds
  (`e5_dialog_after_close_tap.png`) all failed to visibly dismiss the
  `JjChangeGraphScreen` detail `AlertDialog` — all four screenshots are
  indistinguishable from the open state.

  However, a **deterministic** in-process check
  (`graphDialogDismissesViaComposeClick`, using Compose's own test-click
  dispatch rather than external `adb input`) confirms the dialog's real
  dismiss logic is correct: `adb logcat` shows
  `DeviceHarness: dialog-dismiss-via-compose-click stillOpen=false`. A
  control experiment also confirms external `adb shell input tap` *does*
  generally reach this harness's window (tapping "Demo output" on the
  Terminal screen from outside the process correctly triggered real
  output — `f1_external_tap_test.png`). The combination (Compose-internal
  click works; general external tap works elsewhere; external
  tap/key specifically on a second `Dialog` window and on a focused
  `EditText` does not) points to a test-harness input-targeting quirk
  (likely specific to a bare `createComposeRule()`-launched
  `ComponentActivity` versus a fully-composed app window with a real
  `OnBackPressedDispatcher`/IME session), not a demonstrated production
  defect. **This needs a follow-up check directly against the real,
  fully-launched app** (once Connection is reachable, e.g. via a real
  relayd deployment or a credential-provider-equipped device) before
  either passing or failing this specific sub-item.

---

## 3. DeX / external-display behavior — **gap found**

Genymotion does not simulate Samsung DeX specifically. Used
`adb shell wm size 1920x1080` / `wm density 160` (desktop-shaped, reset
afterward with `wm size reset` / `wm density reset`, confirmed via
`adb shell wm size`).

**Evidence:** `i1_fleet_dex.png` (Fleet drawer), `i2_explorer_dex.png`
(Explorer), `i3_terminal_dex.png` (Terminal with real content).

**Gap — no adaptive layout; fixed-phone-width content stretched into a
desktop window with no reflow.** At 1920×1080, both the Fleet drawer
(`i1_fleet_dex.png`) and Explorer (`i2_explorer_dex.png`) render their
list content pinned to the top-left in roughly the same absolute pixel
width as the phone layout, leaving well over 1500px of the window as
dead, unused gray space — no master-detail split, no multi-column grid,
no `WindowSizeClass`-driven layout choice anywhere in the codebase
(`grep -rl "WindowSizeClass\|adaptive" android/app/src/main/java` returns
nothing). This matches the task's own framing of "bottom nav that
doesn't make sense at desktop aspect ratio" — here it's the single-pane
list/drawer pattern that doesn't make sense at desktop aspect ratio.

**Gap — Terminal's PTY grid is hardcoded, not derived from the actual
surface size.** `i3_terminal_dex.png` shows the terminal *does* fill the
full window width (unlike Fleet/Explorer), but the glyphs are rendered
enormous — because `TerminalScreen.kt` calls
`session.create(cols = 80, rows = 24)` unconditionally, regardless of the
real pixel dimensions of the `SurfaceView` it's about to attach to. On a
much larger surface, the same fixed 80×24 grid is simply drawn at a much
larger per-cell size rather than showing more columns/rows of real
content — the opposite of what a resizable desktop terminal should do.
This also means a real PTY attached via `attachPty` would be told it has
an 80×24 window even on a desktop-sized display, which is wrong
information to hand a shell/TUI. Action: derive `cols`/`rows` from the
actual measured surface size and font metrics before calling
`session.create`, and re-issue on real resize (as
`terminal-experience.md`'s "Live font/cell metrics... use for PTY sizing"
already requires).

**Observed, not app-attributable:** at this resolution Android's
large-screen system taskbar (pinned-app dock) appears at the bottom of
the screen (visible in `i1_fleet_dex.png`/`i2_explorer_dex.png`); this is
system, not app, UI, and did not visibly clip any Choosh content in this
pass, but is worth keeping in mind for any future bottom-anchored app UI.

---

## 4. Tablet layout — **gap found (same root cause as item 3)**

Used `adb shell wm size 1600x2560` / `wm density 320` (reset afterward
the same way).

**Evidence:** `j1_fleet_tablet.png` (Fleet drawer), `j2_terminal_tablet.png`
(Terminal with real content).

Same two gaps as item 3, reproduced at the tablet aspect ratio:
Fleet drawer content occupies a small top-left region of a 1600×2560
window with no adaptive use of the extra width or height
(`j1_fleet_tablet.png`); the terminal again fills the surface but keeps
its hardcoded 80×24 grid, so tablet users get the same content at a
larger, coarser cell size rather than more visible terminal content
(`j2_terminal_tablet.png`). No tablet-specific layout path exists in the
codebase. Action: same as item 3 — this is one underlying "no adaptive
layout primitives anywhere in the app" gap, not two separate ones.

---

## 5. Low-memory device profile under sustained terminal output — **pass**

**Evidence:**
- `meminfo_timeline.txt`: `adb shell dumpsys meminfo ai.choosh` `TOTAL`
  PSS sampled every 6s across the full sustained-output run.
- `adb logcat` markers (`DeviceHarness: sustained-progress iteration=N`)
  confirming all 320 iterations of the "Full redraw" demo-injection
  button completed without the process dying.
- A broad `adb logcat` sweep across the full session for
  `FATAL EXCEPTION`, `ANR in ai.choosh`, and `OutOfMemory`: zero matches.
- Source: `rust/choosh-android-bridge/src/terminal_renderer.rs`'s
  `row_cache: Vec<Vec<CellRun>>` is truncated to `total_rows` on every
  resize (`self.row_cache.truncate(total_rows)`), and the glyphon
  `TextAtlas` is trimmed every frame (`self.atlas.trim()`);
  `rust/choosh-terminal-engine/src/grid.rs`/`terminal.rs` confirm the
  terminal engine holds "only the visible viewport, never a scrollback
  buffer" — Zellij, not the Android client, owns real history.

**What was driven:** the `terminalSustainedOutputMemoryProbe` harness
test repeatedly taps the Terminal screen's real "Full redraw" demo-output
button (a full-screen ANSI 256-color clear+redraw, the same real
VT-parser → damage-cache → glyphon render path a live PTY stream would
exercise) 320 times over ~96 seconds (300ms between taps), while an
external process polled `dumpsys meminfo` every 6 seconds.

**Result:** `TOTAL` PSS fluctuated in a narrow band, roughly
321,000–333,000 KB (≈314–325 MB), with no monotonic growth trend across
the ~100-second run (see `meminfo_timeline.txt` for the full 17-sample
series). No crash, no ANR, no dropped frames observable in logcat. This
is consistent with the source-level finding that the row-damage cache is
bounded to the current screen's row count and the glyph atlas is
actively trimmed — memory is genuinely bounded under sustained,
high-volume, colorful output, matching `terminal-experience.md`'s "bound
glyph-atlas, scrollback, frame-queue... memory" requirement.

**Bonus item (constraining available device memory) — not attempted.**
This Genymotion cloud instance reports 16GB total RAM
(`adb shell cat /proc/meminfo`); instance sizing is not something this
pass could adjust via `adb`, and cgroup-level memory constraining wasn't
attempted given the task's own guidance to prioritize the sustained-output
check if time is limited. The sustained-output result above is the
primary evidence for this item; a literal low-memory device profile is a
named, tracked gap in coverage, not a finding of a problem.

---

## Bugs found along the way (real, device-verified defects)

### A. `FakeChooshEngine.webauthnRegisterStart()` crashed the app on the very first real-device tap — **fixed**

Tapping "Set up with a passkey" on the real device crashed the entire
app with:

```
java.lang.IllegalArgumentException: user.name must be defined in requestJson
	at androidx.credentials.CreatePublicKeyCredentialRequest$Companion.getRequestDisplayInfo$credentials(CreatePublicKeyCredentialRequest.kt:201)
	at ai.choosh.connection.ConnectionScreenKt.runRegistrationCeremony(ConnectionScreen.kt:96)
```

`FakeChooshEngine.webauthnRegisterStart()` returned
`{"challenge":"fake-challenge","rp":{"id":"choosh.local"}}` — not a
well-formed WebAuthn `PublicKeyCredentialCreationOptions` JSON (missing
`user`, `rp.name`, `pubKeyCredParams`). `androidx.credentials`'
`CreatePublicKeyCredentialRequest` constructor validates this eagerly
and throws `IllegalArgumentException`, which is **not** a
`CreateCredentialException` and so is not caught by `ConnectionScreen`'s
`catch (failure: CreateCredentialException)` block — it propagates and
kills the app. This reproduces on **any** real device (not specific to
this sandbox's missing credential providers) the first time a user with
no stored credential taps the passkey button. Fixed in
`android/app/src/main/java/ai/choosh/engine/FakeChooshEngine.kt` by
returning a spec-valid fixture JSON; confirmed fixed live
(`dev04_after_passkey_tap.png` shows the correct
`ConnectionUiState.Error("No create options available.")` path instead
of a crash). This is a test-fixture-only change (`FakeChooshEngine`,
never the production `NativeChooshEngine` path), low-risk, and left in
place.

### B. Terminal hardware-keyboard input is entirely unwired — see item 2 above.

### C. Terminal's PTY grid size is hardcoded, not surface-derived — see items 3/4 above.

### D. Systemic missing `mergeDescendants`/`contentDescription` on custom clickable rows — see item 1, Gap 1.

### E. An auth-ceremony bypass button was added, then explicitly rejected and reverted

Mid-pass, a "Skip ceremony (dev/testing only)" button was added to
`ConnectionScreen.kt` to work around this sandbox's missing credential
provider. This was reverted (by direction, given this codebase's
standing passkey-only/no-bypass security posture) before continuing; the
`android/app/src/androidTest/java/ai/choosh/DeviceVerificationHarnessTest.kt`
approach described in "Methodology note" above was used instead, since
it exercises real production screens without touching any
production/auth code path. Noted here for the record, not as an open
gap — `git diff` against `ConnectionScreen.kt` is clean.

---

## Summary table

| Item | Verdict | Primary evidence |
| --- | --- | --- |
| 1. TalkBack pass | Gap | `a1`–`a4`, `b1`, `b3`, `h1` `uiautomator` dumps — unlabeled clickable rows; unlabeled Terminal surface |
| 2. Hardware keyboard | Gap (Terminal); inconclusive (Tab/Escape) | `b1`–`b4` before/after screenshots; source grep; `e1`–`e5`, `g1`–`g2` |
| 3. DeX/external display | Gap | `i1`–`i3` at 1920×1080 |
| 4. Tablet layout | Gap | `j1`–`j2` at 1600×2560 |
| 5. Low-memory / sustained output | Pass | `meminfo_timeline.txt`, logcat markers, source bounds |
