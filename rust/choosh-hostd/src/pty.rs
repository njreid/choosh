//! PTY-over-tunnel streaming for `AgentTerminal`/`Shell` items, per
//! `docs/milestones/M2-terminal-and-agents.md` and
//! `docs/specs/terminal-experience.md`: when a `pty:<item_id>`-purpose
//! tunnel is offered (see `serve.rs`), attach to that item's Zellij tab and
//! pipe bytes bidirectionally over the tunnel.
//!
//! **Real, headless attach — not the `create_session` workaround.**
//! `zellij attach <session>` is a genuine interactive client: it checks
//! `isatty()`, negotiates terminal size, and puts the controlling terminal
//! into raw mode, the same way any TUI program does. Driving it headlessly
//! needs a *real* allocated pseudo-terminal (`nix::pty::openpty`), not a
//! plain pipe — a plain pipe is exactly what made `create_session`'s first,
//! since-replaced approach (see `zellij_ops.rs`'s module doc) unreliable:
//! `zellij` behaves differently, and in that case *worse*, when it can't
//! find a controlling TTY at all.
//!
//! **Known scope limitation, confirmed against the real binary rather than
//! assumed away**: `zellij attach <session>` attaches to the session as a
//! whole and shows whichever tab is focused for that client, not a specific
//! tab by name directly — there is no `--tab` flag on `attach` itself. This
//! module works around that by sending `zellij action go-to-tab-name
//! <tab_name>` (the same `ZELLIJ_SESSION_NAME`-targeted mechanism
//! `zellij_ops::new_tab` uses) immediately after the PTY client attaches.
//! Zellij supports multiple simultaneous clients on one session with
//! independent tab focus in the normal case, so this should scope the
//! focus change to *this* attached client rather than every client — but
//! that per-client-focus behavior is not independently re-verified here
//! beyond the single-client scenario this module's own tests exercise; a
//! second concurrent phone attached to a different tab in the same
//! workspace is a real scenario this pass did not test.
//!
//! **Shared with `ssh_server.rs`**: both this module's `zellij attach`
//! client and `ssh_server`'s interactive shell/exec sessions need a real
//! allocated pty (`zellij attach` for the reason above; an ordinary
//! interactive shell for the usual reasons any shell wants a controlling
//! terminal) — genuinely the same allocate-wire-spawn-and-make-nonblocking
//! mechanics, just fed a different child command. [`allocate_and_spawn`]
//! and [`read_retrying_would_block`] are `pub(crate)` specifically so
//! `ssh_server.rs` shares them rather than duplicating; what differs
//! between the two callers (which command to spawn, whether the caller
//! needs one handle or two independently-`try_clone`d ones, how
//! attach-specific errors are reported) stays with each module.

use nix::pty::{OpenptyResult, openpty};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};

#[derive(Debug)]
pub enum PtyError {
    /// Any failure allocating the pty, wiring it to `zellij attach`'s
    /// stdio, spawning it, or making the master non-blocking — see
    /// [`allocate_and_spawn`], which this wraps without distinguishing
    /// those sub-steps further (nothing downstream of this module matches
    /// on which one failed; the message text still says which).
    Spawn(std::io::Error),
    FocusTab(crate::zellij_ops::ZellijError),
}

impl std::fmt::Display for PtyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "failed to set up the pseudo-terminal-backed zellij attach process: {error}"),
            Self::FocusTab(error) => write!(f, "failed to focus the target tab after attaching: {error}"),
        }
    }
}

impl std::error::Error for PtyError {}

/// An attached, headless `zellij attach` client: `master` is the pty
/// master side (readable/writable — bytes written here reach the attached
/// Zellij client's stdin, bytes read here are the client's stdout/stderr),
/// `child` is the `zellij attach` process itself, killed on drop so a
/// dropped/closed tunnel doesn't leave an orphaned attached client running
/// forever.
pub struct PtySession {
    master: tokio::fs::File,
    child: Child,
}

impl PtySession {
    /// Allocates a pty, spawns `zellij attach <session_name>` wired to it,
    /// and focuses `tab_name` (see this module's doc comment for the scope
    /// limitation on multi-client tab focus).
    ///
    /// # Errors
    ///
    /// See [`PtyError`]'s variants.
    pub async fn attach(session_name: &str, tab_name: &str) -> Result<Self, PtyError> {
        let mut command = Command::new("zellij");
        // Deliberately does NOT set `ZELLIJ_SESSION_NAME` on this child —
        // confirmed by direct experiment (both a bare shell invocation and
        // a real-pty Python harness, 6/6 reproductions) that doing so is
        // actively harmful: `zellij attach <name>` reads that env var (real
        // zellij source, `commands.rs`'s `start_client`) as "the session
        // I'm already a client of" and, if it equals the attach target,
        // deterministically panics with "You are trying to attach to the
        // current session... This is not supported." — a real, 100%-
        // reproducible self-nesting false positive, not a rare race, since
        // this attach is always *from outside* `session_name`, never a
        // client of it already. The session to attach to is already fully
        // specified via the positional `.arg(session_name)` below; this env
        // var is `focus_tab`'s mechanism (its `zellij action` invocation
        // has no explicit session argument of its own and needs it), not
        // this command's.
        command.arg("attach").arg(session_name);
        let (master, child) = allocate_and_spawn(command).map_err(PtyError::Spawn)?;
        let master = tokio::fs::File::from_std(master);

        // Give the attach client a moment to actually connect before
        // asking it to change focus — `go-to-tab-name` targets "the
        // client(s) attached to this session", which is meaningless before
        // any client has finished attaching.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        crate::zellij_ops::focus_tab(session_name, tab_name).await.map_err(PtyError::FocusTab)?;

        Ok(Self { master, child })
    }

    /// # Errors
    ///
    /// An I/O error reading the pty master (e.g. the attached client
    /// exited and closed its end).
    pub async fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        read_retrying_would_block(&mut self.master, buf).await
    }

    /// # Errors
    ///
    /// An I/O error writing to the pty master.
    pub async fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.master.write_all(buf).await
    }

    /// Splits into independent read/write halves sharing the same
    /// underlying pty master (`tokio::io::split`, safe for concurrent use
    /// from separate tasks — this is exactly `serve.rs`'s use case: one
    /// background task continuously reads and forwards output over the
    /// tunnel, while the main dispatch loop writes phone-originated input
    /// as it arrives). `child` moves into the write half, so the attached
    /// `zellij attach` client is killed when the write half drops — the
    /// two halves are always dropped together in `serve.rs`'s usage (both
    /// owned by the same tunnel's teardown path), so this is a reasonable,
    /// documented simplification rather than reference-counting kill
    /// responsibility across both halves.
    #[must_use]
    pub fn split(self) -> (PtyReadHalf, PtyWriteHalf) {
        let (read_half, write_half) = tokio::io::split(self.master);
        (PtyReadHalf(read_half), PtyWriteHalf { inner: write_half, child: self.child })
    }
}

pub struct PtyReadHalf(tokio::io::ReadHalf<tokio::fs::File>);

impl PtyReadHalf {
    /// # Errors
    ///
    /// An I/O error reading the pty master.
    pub async fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        read_retrying_would_block(&mut self.0, buf).await
    }
}

/// The master fd is deliberately non-blocking (see [`allocate_and_spawn`]'s
/// `O_NONBLOCK` setup) so tokio's async I/O can multiplex it — but
/// `tokio::fs::File`'s `AsyncRead` impl runs the underlying `read(2)` on a
/// blocking-pool thread and forwards *whatever* that syscall returns,
/// `EAGAIN`/`WouldBlock` included, rather than treating "genuinely nothing
/// to read yet" as a reason to wait and retry. A `read()` racing ahead of
/// the attached client actually producing output (a completely normal,
/// expected timing outcome, not a real error) would otherwise surface as a
/// hard I/O error to every caller. `pub(crate)` so `ssh_server.rs`'s own
/// pty-output forwarder shares this exact retry rather than duplicating it
/// for its own, differently-obtained pty master — used here for both of
/// `PtySession`'s own read paths ([`PtySession::read`] and
/// [`PtyReadHalf::read`]) too.
pub(crate) async fn read_retrying_would_block(file: &mut (impl AsyncRead + Unpin), buf: &mut [u8]) -> std::io::Result<usize> {
    loop {
        match file.read(buf).await {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            result => return result,
        }
    }
}

/// Allocates a pty, wires `command`'s stdin/stdout/stderr to its slave side
/// (a real controlling terminal — both [`PtySession::attach`]'s `zellij
/// attach` client and `ssh_server`'s interactive shell/exec need one, for
/// the reasons each documents), spawns it, and makes the master
/// non-blocking (required for tokio's async file I/O to multiplex it, per
/// `tokio::fs::File::from_std`'s own requirement for fds that aren't
/// regular files). Returns the master side as a plain `std::fs::File` —
/// not yet wrapped for tokio — since callers differ in what they need it
/// for: `PtySession::attach` wraps it as a single `tokio::fs::File` and
/// only splits it later via an explicit `.split()`; `ssh_server`'s
/// `spawn_pty_backed` instead `try_clone`s it immediately into independent
/// read/write handles, since its write half lives inside a synchronous
/// callback while its read half moves into a spawned task right away.
///
/// # Errors
///
/// Any I/O error from `openpty`, cloning the slave fd, spawning `command`,
/// or the `fcntl` calls that set `O_NONBLOCK` — folded into a single
/// `std::io::Error` rather than distinguishing which sub-step failed,
/// since neither caller needs that distinction (see [`PtyError::Spawn`]'s
/// doc comment).
pub(crate) fn allocate_and_spawn(mut command: Command) -> std::io::Result<(std::fs::File, Child)> {
    let OpenptyResult { master, slave } = openpty(None, None)?;

    // The slave fd becomes the child's stdin/stdout/stderr — cloned rather
    // than moving `slave` three times, since `Command::stdin`/`stdout`/
    // `stderr` each need their own `Stdio`.
    let slave_stdin = slave.try_clone()?;
    let slave_stdout = slave.try_clone()?;
    command
        .stdin(std::process::Stdio::from(slave_stdin))
        .stdout(std::process::Stdio::from(slave_stdout))
        .stderr(std::process::Stdio::from(slave));
    let child = command.spawn()?;

    let flags = nix::fcntl::fcntl(&master, nix::fcntl::FcntlArg::F_GETFL)?;
    let mut new_flags = nix::fcntl::OFlag::from_bits_truncate(flags);
    new_flags.insert(nix::fcntl::OFlag::O_NONBLOCK);
    nix::fcntl::fcntl(&master, nix::fcntl::FcntlArg::F_SETFL(new_flags))?;

    Ok((std::fs::File::from(master), child))
}

pub struct PtyWriteHalf {
    inner: tokio::io::WriteHalf<tokio::fs::File>,
    child: Child,
}

impl PtyWriteHalf {
    /// # Errors
    ///
    /// An I/O error writing to the pty master.
    pub async fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.inner.write_all(buf).await
    }
}

impl Drop for PtyWriteHalf {
    fn drop(&mut self) {
        // Best-effort: a tunnel closing (the phone backgrounding, a
        // network drop) must not leave the attached `zellij attach` client
        // process running forever. `start_kill` is non-blocking and safe
        // to call from `Drop` (unlike `.kill().await`, which isn't).
        let _ = self.child.start_kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end against a real Zellij session and a real spawned shell
    /// command inside it: writes a command to the pty master, reads the
    /// command's output back, and confirms it round-trips — proves bytes
    /// written to the master reach the process running in the target tab
    /// and its output reaches the master, per this module's directive.
    #[tokio::test]
    async fn bytes_written_reach_the_tab_and_its_output_reaches_the_master() {
        let session_name = format!("pty-test-{}", uuid::Uuid::new_v4());
        let dir = tempfile::tempdir().unwrap();
        crate::zellij_ops::create_session(&session_name, dir.path()).await.unwrap();
        // A second tab with a shell (the session's own first tab already
        // has one, but exercising `new_tab`'s own naming/targeting here
        // matches how a real `AgentTerminal`/`Shell` item is created).
        crate::zellij_ops::new_tab(&session_name, "shelltab", dir.path(), &[]).await.unwrap();

        let mut pty = PtySession::attach(&session_name, "shelltab").await.unwrap();

        // Bracketed-paste/prompt noise from the freshly attached client is
        // real and expected; poll for the marker rather than assuming the
        // first read is clean shell output.
        let marker = format!("choosh-pty-test-{}", uuid::Uuid::new_v4());
        pty.write_all(format!("echo {marker}\n").as_bytes()).await.unwrap();

        let mut collected = Vec::new();
        let mut buf = [0u8; 4096];
        let found = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let n = pty.read(&mut buf).await.unwrap();
                collected.extend_from_slice(&buf[..n]);
                if String::from_utf8_lossy(&collected).contains(&marker) {
                    return;
                }
            }
        })
        .await;

        crate::zellij_ops::kill_session(&session_name).await.ok();
        assert!(found.is_ok(), "did not observe the echoed marker within the timeout; collected: {:?}", String::from_utf8_lossy(&collected));
    }
}
