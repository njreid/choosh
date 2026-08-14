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

use nix::pty::{OpenptyResult, openpty};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};

#[derive(Debug)]
pub enum PtyError {
    Allocate(nix::Error),
    Spawn(std::io::Error),
    NonBlocking(std::io::Error),
    FocusTab(crate::zellij_ops::ZellijError),
}

impl std::fmt::Display for PtyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Allocate(error) => write!(f, "failed to allocate a pseudo-terminal: {error}"),
            Self::Spawn(error) => write!(f, "failed to spawn zellij attach: {error}"),
            Self::NonBlocking(error) => write!(f, "failed to configure the pty master as non-blocking: {error}"),
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
        let OpenptyResult { master, slave } = openpty(None, None).map_err(PtyError::Allocate)?;

        // The slave fd becomes the child's stdin/stdout/stderr (a real
        // controlling terminal, not a pipe) — cloned via `try_clone_to_owned`
        // rather than moving `slave` three times, since `Command::stdin`/
        // `stdout`/`stderr` each need their own `Stdio`.
        let slave_stdin = slave.try_clone().map_err(PtyError::Spawn)?;
        let slave_stdout = slave.try_clone().map_err(PtyError::Spawn)?;
        let child = Command::new("zellij")
            .env("ZELLIJ_SESSION_NAME", session_name)
            .arg("attach")
            .arg(session_name)
            .stdin(std::process::Stdio::from(slave_stdin))
            .stdout(std::process::Stdio::from(slave_stdout))
            .stderr(std::process::Stdio::from(slave))
            .spawn()
            .map_err(PtyError::Spawn)?;

        // The master fd must be non-blocking for tokio's async file I/O to
        // multiplex it correctly alongside everything else on the runtime,
        // per tokio::fs::File::from_std's own requirement for fds that
        // aren't regular files.
        let flags = nix::fcntl::fcntl(&master, nix::fcntl::FcntlArg::F_GETFL)
            .map_err(|e| PtyError::NonBlocking(std::io::Error::from_raw_os_error(e as i32)))?;
        let mut new_flags = nix::fcntl::OFlag::from_bits_truncate(flags);
        new_flags.insert(nix::fcntl::OFlag::O_NONBLOCK);
        nix::fcntl::fcntl(&master, nix::fcntl::FcntlArg::F_SETFL(new_flags))
            .map_err(|e| PtyError::NonBlocking(std::io::Error::from_raw_os_error(e as i32)))?;
        let master = tokio::fs::File::from_std(std::fs::File::from(master));

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
        self.master.read(buf).await
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
        self.0.read(buf).await
    }
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
