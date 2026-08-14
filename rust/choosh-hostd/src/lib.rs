//! `choosh-hostd`: the devhost daemon (`serve`) and laptop bridge (`proxy`).
//! See `docs/specs/auth-and-enrollment.md`, `docs/specs/relay-protocol.md`,
//! and `docs/specs/host-deployment.md` for the behavior this crate
//! implements, and `DESIGN.md` §6 for the surrounding architecture.
//!
//! This is the M0 scaffold: the CLI shape is fixed and parses correctly;
//! subcommand bodies land in follow-up increments per
//! `docs/milestones/M0-enrollment.md`.

mod agent_launch;
mod backoff;
pub mod credential;
mod frame_channel;
pub mod fs_ops;
pub mod hooks;
pub mod jj_ops;
pub mod local_ipc;
pub mod pty;
pub mod registry;
pub mod rpc;
pub mod serve;
mod zellij_ops;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "choosh-hostd", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Devhost daemon mode: enroll on first run (via `CHOOSH_ENROLLMENT_TOKEN`
    /// if no device credential is yet persisted), then dial `relayd`
    /// outbound and stay connected.
    Serve,
    /// Laptop bridge mode: no daemon, no Zellij, no workspace registry.
    Proxy {
        #[command(subcommand)]
        command: ProxyCommand,
    },
    /// Invoked by an installed observational agent hook (see
    /// `docs/specs/agent-events.md`), never by a human. Reads the hook's
    /// raw JSON payload from stdin, normalizes it per `--surface`, and
    /// forwards the result to the running `serve` daemon over the local
    /// IPC socket. Ignores (exits 0, does nothing) any session missing a
    /// complete `CHOOSH_WORKSPACE_ID`/`CHOOSH_ITEM_ID`/`CHOOSH_ROOT`/
    /// `CHOOSH_AGENT` environment, per the adapter contract's explicit
    /// requirement — this is not an error, most hook invocations on a
    /// devhost are for sessions Choosh never launched.
    Emit {
        #[arg(long)]
        surface: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum ProxyCommand {
    /// Exchange a one-shot enrollment token for this laptop's device
    /// credential. Run once.
    Enroll {
        #[arg(long)]
        token: String,
    },
    /// The literal `ProxyCommand` target for `~/.ssh/config`.
    Connect {
        host_id: String,
    },
    /// Refresh `~/.ssh/known_hosts` and `~/.ssh/config` from `relayd`'s
    /// relay-attested fleet list.
    Sync,
}

/// # Errors
///
/// Returns an error if the requested subcommand fails. `proxy` variants are
/// currently unimplemented scaffolding pending their own increment
/// (`choosh-hostd proxy` is explicitly out of M0's scope — see
/// `docs/milestones/M0-enrollment.md`'s non-goals).
pub async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();

    match cli.command {
        Command::Serve => serve::run().await.map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>),
        Command::Proxy { command } => match command {
            ProxyCommand::Enroll { token: _ } => {
                Err("choosh-hostd proxy enroll: not yet implemented".into())
            }
            ProxyCommand::Connect { host_id: _ } => {
                Err("choosh-hostd proxy connect: not yet implemented".into())
            }
            ProxyCommand::Sync => Err("choosh-hostd proxy sync: not yet implemented".into()),
        },
        Command::Emit { surface } => run_emit(&surface).await,
    }
}

/// # Errors
///
/// Only for genuine failures (stdin unreadable, local IPC send failure with
/// `serve` apparently running but unreachable) — an incomplete Choosh
/// environment or an unrecognized surface are both quiet no-ops (`Ok(())`),
/// per `Command::Emit`'s doc comment.
async fn run_emit(surface: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use std::io::Read;

    let (Ok(workspace_id), Ok(item_id), Ok(root), Ok(agent_str)) = (
        std::env::var("CHOOSH_WORKSPACE_ID"),
        std::env::var("CHOOSH_ITEM_ID"),
        std::env::var("CHOOSH_ROOT"),
        std::env::var("CHOOSH_AGENT"),
    ) else {
        return Ok(()); // incomplete Choosh environment — silently ignored, per the adapter contract.
    };
    let agent = match agent_str.as_str() {
        "codex" => choosh_protocol::agent_event::AgentAdapter::Codex,
        "claude" => choosh_protocol::agent_event::AgentAdapter::Claude,
        "opencode" => choosh_protocol::agent_event::AgentAdapter::OpenCode,
        _ => return Ok(()), // unrecognized CHOOSH_AGENT value — not this adapter's session.
    };

    // Bounded read: MAX_CAPTURE_BYTES + 1 so an oversized payload is
    // detectably over the limit (and rejected by hooks::normalize's
    // underlying validation) rather than silently truncated into
    // something that happens to still parse as valid, smaller JSON.
    let mut raw_payload = Vec::new();
    std::io::stdin().take(u64::try_from(choosh_protocol::agent_event::MAX_CAPTURE_BYTES).unwrap_or(u64::MAX) + 1).read_to_end(&mut raw_payload)?;

    let occurred_at = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string());
    let input = hooks::EmitInput { workspace_id: &workspace_id, item_id: &item_id, root: &root, agent, surface, occurred_at: &occurred_at, raw_payload: &raw_payload };

    let Ok(normalized) = hooks::normalize(&input) else {
        return Ok(()); // an unrecognized surface, or a validation failure — not a fatal emit error, see agent_event::normalize_*'s own doc comments.
    };
    let wire_event = hooks::to_wire_event(&normalized);

    let socket_path = local_ipc::default_socket_path()?;
    local_ipc::send_event(&socket_path, &wire_event).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_serve() {
        let cli = Cli::parse_from(["choosh-hostd", "serve"]);
        assert!(matches!(cli.command, Command::Serve));
    }

    #[test]
    fn cli_parses_proxy_enroll() {
        let cli = Cli::parse_from(["choosh-hostd", "proxy", "enroll", "--token", "abc"]);
        assert!(matches!(
            cli.command,
            Command::Proxy {
                command: ProxyCommand::Enroll { token }
            } if token == "abc"
        ));
    }

    #[test]
    fn cli_parses_proxy_connect() {
        let cli = Cli::parse_from(["choosh-hostd", "proxy", "connect", "build-box"]);
        assert!(matches!(
            cli.command,
            Command::Proxy {
                command: ProxyCommand::Connect { host_id }
            } if host_id == "build-box"
        ));
    }
}
