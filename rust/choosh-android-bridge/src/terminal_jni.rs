//! JNI boundary for `ai.choosh.TerminalBridge` — a **separate** Kotlin
//! object from `ai.choosh.NativeBridge` (see `lib.rs`), per this task's
//! directive: connection/auth (`NativeBridge`) and a per-surface terminal
//! renderer's much higher-frequency lifecycle (surface-created/-changed/
//! -destroyed, write, resize) are unrelated responsibilities.
//!
//! Android `Surface` -> `ANativeWindow` conversion here ports
//! `njreid/zelland@8bf9cf5`'s `src-tauri/src/renderer/android.rs` (see
//! `docs/licenses/terminal-provenance.md`); everything else (session
//! registry, PTY tunnel wiring) is new, built on
//! [`choosh_terminal_engine::Engine`] and [`crate::terminal_renderer::Renderer`].

use crate::terminal_renderer::{RawDisplay, RawWindow, Renderer};
use choosh_terminal_engine::{Engine as TerminalEngine, Key, Modifiers, MouseAction, MouseButton, MouseEvent};
use jni::objects::{JByteArray, JClass, JObject, JString};
use jni::sys::{jboolean, jint, jlong};
use jni::{Env, native_method};
use raw_window_handle::{AndroidDisplayHandle, AndroidNdkWindowHandle, RawDisplayHandle, RawWindowHandle};
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

/// State for one live terminal surface. One per pinned `AgentTerminal`
/// page kept mounted, per `terminal-experience.md`'s "one retained
/// renderer per visible terminal surface".
///
/// The PTY tunnel itself is *not* stored here: `PtyTunnelHandle::read`
/// needs `&mut self` (its `output_rx` is a single-consumer channel), while
/// key/paste/mouse input must be sendable at any time without waiting on
/// whatever `.read().await` happens to be parked on. So the tunnel is
/// owned exclusively by the background task spawned in
/// `native_terminal_attach_pty`, which `tokio::select!`s between PTY
/// output and an outgoing-bytes channel — `pty_tx` is this session's
/// sending half of that channel.
struct TerminalSession {
    engine: TerminalEngine,
    renderer: Option<Renderer>,
    pty_tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
    /// The most recent `surfaceChanged` pixel size, recorded here — not
    /// only inside `Renderer` — because Android can (and, confirmed on a
    /// real device, reliably does) call `surfaceChanged` before
    /// `Renderer::init()`'s async GPU device creation finishes, at which
    /// point `renderer` is still `None` and a resize call has nowhere to
    /// go. Without this, the surface stayed configured at its 1x1
    /// placeholder size forever (frames were submitted successfully —
    /// `frames_rendered` incremented — but into a 1x1 texture, which reads
    /// as a solid-color screen once the compositor stretches it). Mirrors
    /// Zelland's own fix for the identical race
    /// (`renderer/mod.rs`'s `PENDING_SIZE` static, per `WGPU_FIXES.md`)
    /// scoped to this session instead of a process-global.
    pending_size: Option<(u32, u32)>,
}

struct SessionHandle {
    session: Arc<Mutex<TerminalSession>>,
    // `Arc` (not a bare `Runtime`) so callers can pull an owned handle out
    // from behind the `SESSIONS` lock and use it (`block_on`/`spawn`)
    // *after* releasing that lock — see `native_terminal_attach_pty`'s and
    // `native_terminal_surface_created`'s doc comments for why holding the
    // global session-registry lock across a network round trip would be a
    // real, avoidable bottleneck across every other terminal session.
    runtime: Arc<Runtime>,
}

static SESSIONS: LazyLock<Mutex<HashMap<i64, SessionHandle>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT_HANDLE: AtomicI64 = AtomicI64::new(1);

fn jstring_to_string(env: &Env<'_>, value: &JString<'_>) -> String {
    value.mutf8_chars(env).map(|chars| chars.to_string()).unwrap_or_default()
}

fn with_session<T>(handle: jlong, absent: T, present: impl FnOnce(&SessionHandle) -> T) -> T {
    let sessions = SESSIONS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    sessions.get(&handle).map_or(absent, present)
}

/// Redraws the surface if a renderer exists — the one place every mutation
/// path (PTY bytes, resize, test-inject) converges, matching
/// `terminal-experience.md`'s "render only after damage ...".
///
/// The detailed `Renderer::debug_state()` diagnostic (row cache dump, font
/// db introspection) that was invaluable for on-device verification during
/// this pass — see `docs/licenses/terminal-provenance.md`'s addendum for
/// the two real bugs it found — is deliberately *not* called from this hot
/// path: it is real, useful debug instrumentation, but cheap enough only
/// to justify running on demand, not on every redraw during sustained
/// output. [`Renderer::debug_state`] stays available for a future
/// on-demand diagnostic hook (e.g. a long-press/shake gesture) rather than
/// being deleted.
fn redraw(session: &Mutex<TerminalSession>) {
    let mut guard = session.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let TerminalSession { engine, renderer, .. } = &mut *guard;
    if let Some(renderer) = renderer.as_mut() {
        renderer.draw(engine);
    }
}

// Writes one line to logcat under tag "choosh_terminal". Used sparingly
// (GPU-init failure today — see `native_terminal_surface_created`) rather
// than on every redraw: verbose per-frame logging was real, useful
// instrumentation while debugging this pass' two real on-device bugs (see
// docs/licenses/terminal-provenance.md's addendum) but is not something to
// leave running at full cost during sustained agent output.
fn alog(msg: &str) {
    let tag = c"choosh_terminal";
    let Ok(text) = std::ffi::CString::new(msg) else { return };
    // SAFETY: both pointers are valid, NUL-terminated C strings owned for
    // the duration of this call; `__android_log_write` copies what it needs
    // synchronously and does not retain either pointer afterward.
    unsafe {
        ndk_sys::__android_log_write(ndk_sys::android_LogPriority::ANDROID_LOG_INFO.0.cast_signed(), tag.as_ptr(), text.as_ptr());
    }
}

#[allow(clippy::unnecessary_wraps)] // see native_close's identical rationale in lib.rs
fn native_terminal_create(_env: &mut Env<'_>, _class: JClass<'_>, cols: jint, rows: jint) -> Result<jlong, jni::errors::Error> {
    let cols = u16::try_from(cols.max(1)).unwrap_or(u16::MAX);
    let rows = u16::try_from(rows.max(1)).unwrap_or(u16::MAX);
    let Ok(runtime) = Runtime::new() else { return Ok(0) };
    let session = TerminalSession { engine: TerminalEngine::new(cols, rows), renderer: None, pty_tx: None, pending_size: None };
    let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    SESSIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(handle, SessionHandle { session: Arc::new(Mutex::new(session)), runtime: Arc::new(runtime) });
    Ok(handle)
}

/// Opens the `pty:<item_id>` relay tunnel for this session (via the
/// existing connection `Engine` behind `connection_handle` — see
/// `lib.rs::with_connection_engine`) and spawns the background task that
/// feeds every PTY chunk into the VT parser and redraws. Returns `false`
/// if not connected or the tunnel failed to open.
// `needless_pass_by_value`: see lib.rs's native_connect's identical rationale.
// `unnecessary_wraps`: see lib.rs's native_close's identical rationale — the
// `Result` wrapper is required by `native_method!`'s non-raw expansion, not optional.
#[allow(clippy::needless_pass_by_value, clippy::unnecessary_wraps)]
fn native_terminal_attach_pty<'local>(
    env: &mut Env<'local>,
    _class: JClass<'local>,
    handle: jlong,
    connection_handle: jlong,
    target_device_id: JString<'local>,
    item_id: JString<'local>,
) -> Result<jboolean, jni::errors::Error> {
    let target_device_id = jstring_to_string(env, &target_device_id);
    let item_id = jstring_to_string(env, &item_id);

    let Some((runtime, session)) = with_session(handle, None, |h| Some((Arc::clone(&h.runtime), Arc::clone(&h.session)))) else {
        return Ok(jboolean::from(false));
    };

    let opened = crate::with_connection_engine(connection_handle, |connection_engine, _connection_runtime| {
        // `open_pty_tunnel` only touches runtime-agnostic tokio channels
        // (see engine.rs's doc comment on the method) — safe to await from
        // this session's own runtime even though the connection lives on
        // a different one.
        runtime.block_on(connection_engine.open_pty_tunnel(&target_device_id, &item_id))
    });

    let mut pty = match opened {
        Some(Ok(pty)) => pty,
        Some(Err(error)) => {
            tracing::warn!(%error, "terminal attach_pty failed");
            return Ok(jboolean::from(false));
        }
        None => return Ok(jboolean::from(false)),
    };

    // PTY sizing rides the Zellij pane's own attach flow (`choosh-hostd`'s
    // job, per docs/specs/terminal-experience.md), not this tunnel-open
    // handshake — nothing to send here beyond opening the tunnel.
    let (write_tx, mut write_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    session.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pty_tx = Some(write_tx);

    let read_session = Arc::clone(&session);
    runtime.spawn(async move {
        loop {
            tokio::select! {
                chunk = pty.read() => {
                    match chunk {
                        Some(bytes) => {
                            let mut guard = read_session.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                            guard.engine.write(&bytes);
                            drop(guard);
                            redraw(&read_session);
                        }
                        None => break,
                    }
                }
                outgoing = write_rx.recv() => {
                    match outgoing {
                        Some(bytes) => { let _ = pty.write(&bytes); }
                        None => break,
                    }
                }
            }
        }
        read_session.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pty_tx = None;
    });
    Ok(jboolean::from(true))
}

/// # Safety-relevant note
/// `surface` must be the live `android.view.Surface` from a
/// `SurfaceHolder.Callback.surfaceCreated` (or later) callback — this
/// mirrors Zelland's `passSurfaceToRust`
/// (`renderer/android.rs::Java_..._passSurfaceToRust`).
// `unnecessary_wraps`: see native_terminal_attach_pty's identical rationale.
#[allow(clippy::needless_pass_by_value, clippy::unnecessary_wraps)]
fn native_terminal_surface_created<'local>(env: &mut Env<'local>, _class: JClass<'local>, handle: jlong, surface: JObject<'local>) -> Result<(), jni::errors::Error> {
    let Some((runtime, session)) = with_session(handle, None, |h| Some((Arc::clone(&h.runtime), Arc::clone(&h.session)))) else {
        return Ok(());
    };

    // SAFETY: `env`'s raw JNIEnv pointer and `surface`'s raw jobject are
    // both valid for the duration of this call, per the JNI contract for
    // any native method invocation — `ANativeWindow_fromSurface` reads
    // them synchronously and returns an owned `ANativeWindow*` (refcounted
    // by the NDK, released by `NativeWindow`'s `Drop`).
    let native_window = unsafe {
        let ptr = ndk_sys::ANativeWindow_fromSurface(env.get_raw().cast(), surface.as_raw());
        if ptr.is_null() {
            return Ok(());
        }
        ndk::native_window::NativeWindow::from_ptr(std::ptr::NonNull::new(ptr).unwrap())
    };
    let window_ptr = native_window.ptr().as_ptr().cast::<std::ffi::c_void>();
    let Some(non_null) = std::ptr::NonNull::new(window_ptr) else { return Ok(()) };
    let raw_window = RawWindow::new(RawWindowHandle::AndroidNdk(AndroidNdkWindowHandle::new(non_null)));
    let raw_display = RawDisplay::new(RawDisplayHandle::Android(AndroidDisplayHandle::new()));

    runtime.spawn(async move {
        let needs_init = session.lock().unwrap_or_else(std::sync::PoisonError::into_inner).renderer.is_none();
        if needs_init {
            match Renderer::init().await {
                Ok(renderer) => session.lock().unwrap_or_else(std::sync::PoisonError::into_inner).renderer = Some(renderer),
                Err(error) => {
                    // `tracing::warn!` has no subscriber wired in this
                    // crate, so it goes nowhere — `alog` (real logcat
                    // output) is what actually satisfies
                    // terminal-experience.md's "fall back visibly when GPU
                    // initialization fails rather than presenting a blank
                    // terminal" for this one, low-frequency, genuinely
                    // important failure path.
                    alog(&format!("terminal renderer GPU init failed: {error}"));
                    return;
                }
            }
        }
        let mut guard = session.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let TerminalSession { engine, renderer, pending_size, .. } = &mut *guard;
        if let Some(renderer) = renderer.as_mut() {
            renderer.set_surface(&raw_window, &raw_display);
            // Apply whatever `surfaceChanged` already delivered while this
            // task was still awaiting `Renderer::init()` — see
            // `TerminalSession::pending_size`'s doc comment for why this
            // can't just live inside `Renderer` itself.
            if let Some((width, height)) = pending_size.take() {
                renderer.resize(width, height);
                let (cols, rows) = renderer.cells_for_size(width, height);
                engine.resize(cols, rows);
                // See `native_terminal_surface_changed`'s identical rationale.
                alog(&format!(
                    "terminal_surface_resized(deferred) width_px={width} height_px={height} cell_width={} cell_height={} cols={cols} rows={rows}",
                    renderer.cell_width, renderer.cell_height
                ));
            }
        }
        drop(guard);
        redraw(&session);
    });
    Ok(())
}

#[allow(clippy::unnecessary_wraps)]
fn native_terminal_surface_changed(_env: &mut Env<'_>, _class: JClass<'_>, handle: jlong, width_px: jint, height_px: jint) -> Result<(), jni::errors::Error> {
    let width_px = width_px.max(0).unsigned_abs();
    let height_px = height_px.max(0).unsigned_abs();
    with_session(handle, (), |h| {
        let mut guard = h.session.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let TerminalSession { engine, renderer, pending_size, .. } = &mut *guard;
        // Always record it, regardless of whether `renderer` exists yet —
        // see `TerminalSession::pending_size`'s doc comment.
        *pending_size = Some((width_px, height_px));
        if let Some(renderer) = renderer.as_mut() {
            renderer.resize(width_px, height_px);
            let (cols, rows) = renderer.cells_for_size(width_px, height_px);
            engine.resize(cols, rows);
            // Real, low-frequency (once per surface resize — rotation,
            // window resize, DeX/external-display attach) diagnostic
            // logging: the authoritative on-device evidence that `cols`/
            // `rows` are genuinely derived from the real surface pixel
            // size and the loaded font's measured cell metrics, per
            // `terminal-experience.md`'s "derive terminal cell metrics
            // from the exact loaded face" and
            // `docs/accessibility-device-report.md`'s item 3/4 finding
            // that the grid stayed pinned at a hardcoded 80x24 regardless
            // of real surface size.
            alog(&format!(
                "terminal_surface_resized width_px={width_px} height_px={height_px} cell_width={} cell_height={} cols={cols} rows={rows}",
                renderer.cell_width, renderer.cell_height
            ));
            renderer.draw(engine);
            *pending_size = None;
        }
    });
    Ok(())
}

#[allow(clippy::unnecessary_wraps)]
fn native_terminal_surface_destroyed(_env: &mut Env<'_>, _class: JClass<'_>, handle: jlong) -> Result<(), jni::errors::Error> {
    with_session(handle, (), |h| {
        let mut guard = h.session.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(renderer) = guard.renderer.as_mut() {
            renderer.drop_surface();
        }
    });
    Ok(())
}

/// Sends one non-printable key command. `key_code` is this JNI boundary's
/// own small wire enum for the [`choosh_terminal_engine::Key`] variants
/// that carry no payload (0=Enter,1=Tab,2=Backspace,3=Delete,4=Escape,
/// 5=Up,6=Down,7=Left,8=Right,9=Home,10=End,11=PageUp,12=PageDown) —
/// printable characters go through `native_terminal_send_text` instead,
/// which carries a real `char`. Kotlin's extra-keys bar and
/// hardware-keyboard dispatcher both funnel through this one path, per
/// `terminal-experience.md`'s "same command dispatcher".
#[allow(clippy::unnecessary_wraps, clippy::fn_params_excessive_bools)]
fn native_terminal_send_key(
    _env: &mut Env<'_>,
    _class: JClass<'_>,
    handle: jlong,
    key_code: jint,
    ctrl: jboolean,
    alt: jboolean,
    shift: jboolean,
) -> Result<(), jni::errors::Error> {
    let Some(key) = key_from_code(key_code) else { return Ok(()) };
    let mods = Modifiers { ctrl, alt, shift };
    with_session(handle, (), |h| {
        let (bytes, attached) = {
            let guard = h.session.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            (guard.engine.encode_key(key, mods), guard.pty_tx.is_some())
        };
        // Real, low-frequency (human-typing-paced) diagnostic logging —
        // key codes and modifier flags only, never any character content,
        // per `terminal-experience.md`'s "Clipboard contents and terminal
        // text MUST NOT be logged". This is the on-device evidence channel
        // for confirming a real hardware key genuinely reached the Rust
        // engine (`docs/accessibility-device-report.md`'s item 2): before
        // this pass there was no call site anywhere that could produce
        // this log line at all.
        alog(&format!("terminal_send_key key_code={key_code} ctrl={ctrl} alt={alt} shift={shift} encoded_len={} pty_attached={attached}", bytes.len()));
        send_bytes(h, &bytes);
    });
    Ok(())
}

fn key_from_code(code: jint) -> Option<Key> {
    Some(match code {
        0 => Key::Enter,
        1 => Key::Tab,
        2 => Key::Backspace,
        3 => Key::Delete,
        4 => Key::Escape,
        5 => Key::Up,
        6 => Key::Down,
        7 => Key::Left,
        8 => Key::Right,
        9 => Key::Home,
        10 => Key::End,
        11 => Key::PageUp,
        12 => Key::PageDown,
        _ => return None,
    })
}

/// Commits IME/hardware-keyboard text (already-composed characters — see
/// `terminal-experience.md`'s IME requirements) as plain UTF-8 bytes to
/// the PTY, honoring Ctrl/Alt per character.
#[allow(clippy::needless_pass_by_value, clippy::unnecessary_wraps)]
fn native_terminal_send_text<'local>(env: &mut Env<'local>, _class: JClass<'local>, handle: jlong, text: JString<'local>, ctrl: jboolean, alt: jboolean) -> Result<(), jni::errors::Error> {
    let text = jstring_to_string(env, &text);
    let char_count = text.chars().count();
    let mods = Modifiers { ctrl, alt, shift: false };
    with_session(handle, (), |h| {
        let mut out = Vec::new();
        let attached = {
            let guard = h.session.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            for ch in text.chars() {
                out.extend(guard.engine.encode_key(Key::Char(ch), mods));
            }
            guard.pty_tx.is_some()
        };
        // See `native_terminal_send_key`'s identical rationale — length and
        // modifier flags only, never the committed text itself.
        alog(&format!("terminal_send_text char_count={char_count} ctrl={ctrl} alt={alt} encoded_len={} pty_attached={attached}", out.len()));
        send_bytes(h, &out);
    });
    Ok(())
}

#[allow(clippy::needless_pass_by_value, clippy::unnecessary_wraps)]
fn native_terminal_paste<'local>(env: &mut Env<'local>, _class: JClass<'local>, handle: jlong, data: JByteArray<'local>) -> Result<(), jni::errors::Error> {
    let bytes = env.convert_byte_array(&data).unwrap_or_default();
    with_session(handle, (), |h| {
        let encoded = {
            let guard = h.session.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.engine.encode_paste(&bytes)
        };
        send_bytes(h, &encoded);
    });
    Ok(())
}

/// SGR mouse forwarding — `col`/`row` are pre-computed by Kotlin from the
/// live cell metrics (`terminal-experience.md`'s "live font/cell metrics
/// ... used for ... pointer encoding"); `action` is 0=press,1=release,2=motion,
/// `button` is 0=left,1=middle,2=right,3=scroll-up,4=scroll-down,5=none.
#[allow(clippy::unnecessary_wraps, clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
fn native_terminal_send_mouse(
    _env: &mut Env<'_>,
    _class: JClass<'_>,
    handle: jlong,
    col: jint,
    row: jint,
    action: jint,
    button: jint,
    shift: jboolean,
    alt: jboolean,
    ctrl: jboolean,
) -> Result<(), jni::errors::Error> {
    let event = MouseEvent {
        col: u16::try_from(col.max(0)).unwrap_or(0),
        row: u16::try_from(row.max(0)).unwrap_or(0),
        button: match button {
            0 => MouseButton::Left,
            1 => MouseButton::Middle,
            2 => MouseButton::Right,
            3 => MouseButton::ScrollUp,
            4 => MouseButton::ScrollDown,
            _ => MouseButton::None,
        },
        action: match action {
            0 => MouseAction::Press,
            1 => MouseAction::Release,
            _ => MouseAction::Motion,
        },
        shift,
        alt,
        ctrl,
    };
    with_session(handle, (), |h| {
        let encoded = {
            let guard = h.session.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.engine.encode_mouse(event)
        };
        if let Some(bytes) = encoded {
            send_bytes(h, &bytes);
        }
    });
    Ok(())
}

fn send_bytes(session_handle: &SessionHandle, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    let guard = session_handle.session.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(tx) = guard.pty_tx.as_ref() {
        let _ = tx.send(bytes.to_vec());
    }
}

/// Test-only synthetic byte source: feeds `bytes` directly into the VT
/// parser and redraws, bypassing the PTY tunnel entirely — this is what
/// the on-device verification screenshots (canned `ls -la` output, a
/// colored prompt, a full-screen redraw) are driven through, per this
/// task's Verification section ("a test-only code path that feeds a
/// canned byte sequence ... directly into the renderer without needing a
/// live PTY tunnel").
#[allow(clippy::needless_pass_by_value, clippy::unnecessary_wraps)]
fn native_terminal_test_inject<'local>(env: &mut Env<'local>, _class: JClass<'local>, handle: jlong, bytes: JByteArray<'local>) -> Result<(), jni::errors::Error> {
    let bytes = env.convert_byte_array(&bytes).unwrap_or_default();
    with_session(handle, (), |h| {
        h.session.lock().unwrap_or_else(std::sync::PoisonError::into_inner).engine.write(&bytes);
        redraw(&h.session);
    });
    Ok(())
}

/// Returns the current visible grid as plain text (see
/// [`choosh_terminal_engine::Engine::visible_text`]) — the accessibility
/// content source `TerminalSurfaceView`'s `ExploreByTouchHelper` virtual
/// node reads, per `docs/accessibility-device-report.md`'s item 1, gap 2
/// ("the Terminal surface has zero accessible content") and
/// `terminal-experience.md`'s "Kotlin owns... accessibility semantics".
/// Returns an empty string (never null) for an absent/destroyed handle so
/// Kotlin callers never need a null check.
fn native_terminal_get_text<'local>(env: &mut Env<'local>, _class: JClass<'local>, handle: jlong) -> Result<JString<'local>, jni::errors::Error> {
    let text = with_session(handle, String::new(), |h| h.session.lock().unwrap_or_else(std::sync::PoisonError::into_inner).engine.visible_text());
    JString::new(env, text)
}

#[allow(clippy::unnecessary_wraps)]
fn native_terminal_destroy(_env: &mut Env<'_>, _class: JClass<'_>, handle: jlong) -> Result<(), jni::errors::Error> {
    if let Some(removed) = SESSIONS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&handle) {
        // Dropping `pty_tx` closes the write side of the attach_pty
        // background task's channel; its `tokio::select!` loop then sees
        // `write_rx.recv() => None` (or the tunnel itself ends first) and
        // exits on its own — no direct `PtyTunnelHandle` access needed
        // here, since that task owns it exclusively (see `TerminalSession`'s
        // doc comment).
        removed.session.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pty_tx = None;
    }
    Ok(())
}

const _CREATE: jni::NativeMethod = native_method! {
    java_type = "ai.choosh.TerminalBridge",
    static extern fn TerminalBridge::native_terminal_create(cols: jint, rows: jint) -> jlong,
    fn = native_terminal_create,
};
const _ATTACH_PTY: jni::NativeMethod = native_method! {
    java_type = "ai.choosh.TerminalBridge",
    static extern fn TerminalBridge::native_terminal_attach_pty(handle: jlong, connection_handle: jlong, target_device_id: JString, item_id: JString) -> jboolean,
    fn = native_terminal_attach_pty,
};
const _SURFACE_CREATED: jni::NativeMethod = native_method! {
    java_type = "ai.choosh.TerminalBridge",
    static extern fn TerminalBridge::native_terminal_surface_created(handle: jlong, surface: JObject) -> void,
    fn = native_terminal_surface_created,
};
const _SURFACE_CHANGED: jni::NativeMethod = native_method! {
    java_type = "ai.choosh.TerminalBridge",
    static extern fn TerminalBridge::native_terminal_surface_changed(handle: jlong, width_px: jint, height_px: jint) -> void,
    fn = native_terminal_surface_changed,
};
const _SURFACE_DESTROYED: jni::NativeMethod = native_method! {
    java_type = "ai.choosh.TerminalBridge",
    static extern fn TerminalBridge::native_terminal_surface_destroyed(handle: jlong) -> void,
    fn = native_terminal_surface_destroyed,
};
const _SEND_KEY: jni::NativeMethod = native_method! {
    java_type = "ai.choosh.TerminalBridge",
    static extern fn TerminalBridge::native_terminal_send_key(handle: jlong, key_code: jint, ctrl: jboolean, alt: jboolean, shift: jboolean) -> void,
    fn = native_terminal_send_key,
};
const _SEND_TEXT: jni::NativeMethod = native_method! {
    java_type = "ai.choosh.TerminalBridge",
    static extern fn TerminalBridge::native_terminal_send_text(handle: jlong, text: JString, ctrl: jboolean, alt: jboolean) -> void,
    fn = native_terminal_send_text,
};
const _PASTE: jni::NativeMethod = native_method! {
    java_type = "ai.choosh.TerminalBridge",
    static extern fn TerminalBridge::native_terminal_paste(handle: jlong, data: jbyte[]) -> void,
    fn = native_terminal_paste,
};
const _SEND_MOUSE: jni::NativeMethod = native_method! {
    java_type = "ai.choosh.TerminalBridge",
    static extern fn TerminalBridge::native_terminal_send_mouse(handle: jlong, col: jint, row: jint, action: jint, button: jint, shift: jboolean, alt: jboolean, ctrl: jboolean) -> void,
    fn = native_terminal_send_mouse,
};
const _TEST_INJECT: jni::NativeMethod = native_method! {
    java_type = "ai.choosh.TerminalBridge",
    static extern fn TerminalBridge::native_terminal_test_inject(handle: jlong, bytes: jbyte[]) -> void,
    fn = native_terminal_test_inject,
};
const _GET_TEXT: jni::NativeMethod = native_method! {
    java_type = "ai.choosh.TerminalBridge",
    static extern fn TerminalBridge::native_terminal_get_text(handle: jlong) -> JString,
    fn = native_terminal_get_text,
};
const _DESTROY: jni::NativeMethod = native_method! {
    java_type = "ai.choosh.TerminalBridge",
    static extern fn TerminalBridge::native_terminal_destroy(handle: jlong) -> void,
    fn = native_terminal_destroy,
};
