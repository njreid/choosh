// `jj_ops.rs`'s real `jj-lib` calls (see its module doc comment) add real
// async-call-chain depth on top of this crate's already-deep
// `serve::run()`/`serve_dispatch()` future nesting — enough that rustc's
// default recursion limit (128) overflows while computing that future's
// layout (`query depth increased by 130`, verified against a real build
// after adding `jj-lib` usage). This raises the *compile-time* type-layout
// recursion budget only; it has no runtime effect (the existing
// `Box::pin(choosh_hostd::run(..))` in `main.rs` already handles the
// separate, runtime concern of a large top-level future's stack footprint).
#![recursion_limit = "512"]
//! `choosh-hostd`: the devhost daemon (`serve`) and laptop bridge (`proxy`).
//! See `docs/specs/auth-and-enrollment.md`, `docs/specs/relay-protocol.md`,
//! and `docs/specs/host-deployment.md` for the behavior this crate
//! implements, and `DESIGN.md` §6 for the surrounding architecture.
//!
//! This is the M0 scaffold: the CLI shape is fixed and parses correctly;
//! subcommand bodies land in follow-up increments per
//! `docs/milestones/M0-enrollment.md`.

pub mod agent_event_spool;
mod agent_launch;
pub mod auth_detect;
mod backoff;
pub mod credential;
pub mod dev_exec;
mod frame_channel;
pub mod fs_ops;
mod fs_util;
mod host_tool_bin;
pub mod hooks;
pub mod jj_ops;
pub mod local_ipc;
mod mise_ops;
mod power_assertion;
pub mod proxy;
pub mod pty;
mod readiness;
pub mod registry;
mod relay_client;
pub mod rpc;
pub mod serve;
mod ssh_keys;
pub mod ssh_server;
pub mod update;
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
    /// `docs/specs/service-tunnels.md`'s "Explicit launch" CLI form. Thin
    /// sugar over the exact same `item.create` RPC path `serve` drives on
    /// Android's behalf (`rpc::handle_item_create`) — this does not
    /// reimplement validation/launch, it just builds an `ItemCreate`
    /// request and calls `rpc::dispatch` with it, for local/manual
    /// devhost-side use with no phone or `relayd` connection required.
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// M7's cross-host task offload (`docs/milestones/M7-fleet-and-provisioning.md`):
    /// runs `<command>` on the devhost named by `--host` (its raw
    /// `device_id`, not an alias — see `dev_exec`'s module doc comment for
    /// why), against a matching `jj` revision in a fresh ephemeral
    /// workspace derived from `--workspace`'s registered root on *this*
    /// devhost. Streams the remote command's stdout/stderr to this
    /// process's own stdout/stderr and exits with its exact exit code.
    DevExec {
        #[arg(long)]
        host: String,
        #[arg(long)]
        workspace: String,
        /// Fixed argv after `--`, never a shell string — same
        /// `host-rpc.md` "Command construction" discipline every other
        /// subprocess-launching command in this CLI follows.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
    },
    /// Internal use only — spawned by `choosh-hostd`'s own self-update
    /// path (`crate::update::spawn_rollback_monitor`), never by a human or
    /// any other CLI form. See `crate::update`'s module doc comment for
    /// the full design: polls `--socket`'s local IPC socket for
    /// `--current`'s health within a bounded window, and rolls back to
    /// `--previous` (restarting the service once more) if it never comes
    /// up healthy.
    #[command(hide = true)]
    UpdateMonitor {
        #[arg(long)]
        previous: std::path::PathBuf,
        #[arg(long)]
        current: std::path::PathBuf,
        #[arg(long)]
        socket: std::path::PathBuf,
        #[arg(long)]
        state_file: std::path::PathBuf,
        #[arg(long)]
        version: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum ServiceCommand {
    /// Registers and launches a `WebService` item in an already-registered
    /// `--workspace` (by name, per `service-tunnels.md`'s pseudocode) —
    /// this command deliberately does not also register a workspace, only
    /// an item within one that already exists (via a prior
    /// `workspace.create`, e.g. through `serve` + Android, or a devhost
    /// operator's own prior use of this same registry file).
    Run {
        #[arg(long)]
        workspace: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        port: u16,
        /// V1 supports only `http`, per `service-tunnels.md` ("HTTPS-to-host
        /// support is deferred").
        #[arg(long, default_value = "http")]
        protocol: String,
        /// Fixed argv after `--`, never a shell string — matches
        /// `host-rpc.md`'s "Command construction" requirement for
        /// `item.create`'s own `command` field.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
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
/// Returns an error if the requested subcommand fails — see
/// [`serve::ServeError`] and [`proxy::ProxyError`] for the shapes of what
/// can fail there.
pub async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();

    match cli.command {
        Command::Serve => serve::run().await.map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>),
        Command::Proxy { command } => match command {
            ProxyCommand::Enroll { token } => proxy::enroll(&token).await.map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>),
            ProxyCommand::Connect { host_id } => Box::pin(proxy::connect(&host_id)).await.map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>),
            ProxyCommand::Sync => proxy::sync().await.map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>),
        },
        Command::Emit { surface } => run_emit(&surface).await,
        Command::Service { command: ServiceCommand::Run { workspace, name, port, protocol, command } } => {
            run_service_run(&workspace, &name, port, &protocol, command).await
        }
        Command::DevExec { host, workspace, command } => run_dev_exec(&host, &workspace, command).await,
        Command::UpdateMonitor { previous, current, socket, state_file, version } => {
            update::run_monitor(previous, current, socket, state_file, version).await;
            Ok(())
        }
    }
}

/// # Errors
///
/// Returns an error (exit code 1, per this crate's other subcommands) for
/// every *transport/protocol*-level failure — see [`dev_exec::DevExecError`].
/// A non-zero exit from the *offloaded command itself* is NOT one of these:
/// per M7's exit criterion ("streams output back... " with an implied
/// faithful result), that exit code is propagated verbatim via
/// `std::process::exit`, which this function calls directly rather than
/// returning through the normal `Result` plumbing (whose `Err` path this
/// crate's `main` always turns into a flat exit code 1, per `main.rs`'s
/// `Result`-returning `main` — too coarse for "propagate the exact code").
async fn run_dev_exec(host: &str, workspace: &str, command: Vec<String>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match dev_exec::run(host, workspace, command).await {
        Ok(exit_code) => std::process::exit(exit_code),
        Err(error) => Err(Box::new(error) as Box<dyn std::error::Error + Send + Sync>),
    }
}

/// # Errors
///
/// Returns an error if `--protocol` isn't `"http"`, `--workspace` doesn't
/// name an already-registered workspace, or the underlying `item.create`
/// RPC itself returns an error (name collision, invalid command, etc.) —
/// see `rpc::handle_item_create`.
async fn run_service_run(
    workspace: &str,
    name: &str,
    port: u16,
    protocol: &str,
    command: Vec<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if protocol != "http" {
        return Err(format!(
            "choosh-hostd service run: unsupported --protocol {protocol:?}; V1 supports only \"http\" per service-tunnels.md"
        )
        .into());
    }

    let device_id = serve::best_effort_device_id();
    let ctx = serve::build_rpc_context(&device_id).map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)?;

    let workspace_id = {
        let registry = ctx.registry.lock().await;
        registry
            .find_workspace_by_name(workspace)
            .map(|w| w.workspace_id.clone())
            .ok_or_else(|| format!("workspace {workspace:?} is not registered on this devhost (register it first, e.g. via Android)"))?
    };

    let response = rpc::dispatch(
        &ctx,
        choosh_protocol::host_rpc::RpcRequest::ItemCreate {
            request_id: uuid::Uuid::new_v4().to_string(),
            workspace_id,
            item_type: choosh_protocol::host_rpc::ItemType::WebService,
            name: name.to_string(),
            agent: None,
            command: Some(command),
            port: Some(port),
        },
    )
    .await;

    let item_id = match response {
        choosh_protocol::host_rpc::RpcResponse::ItemCreateOk { item_id, tab_target, .. } => {
            println!("created WebService item {item_id} (tab {tab_target}), status: starting");
            item_id
        }
        choosh_protocol::host_rpc::RpcResponse::Error { code, message, .. } => {
            return Err(format!("item.create failed: {code}: {message}").into());
        }
        other => return Err(format!("item.create returned an unexpected response: {other:?}").into()),
    };

    // `item.create` (above) already spawned the real readiness prober
    // (`readiness::spawn`, inside `rpc::handle_item_create`) as a detached
    // task on this same process's runtime — it keeps running as long as
    // this process's `main` hasn't returned yet, which is true for the
    // rest of this function. Poll the registry to observe its outcome
    // rather than launching a second, redundant probe.
    let wait_bound = readiness::READINESS_TIMEOUT + std::time::Duration::from_secs(2);
    let deadline = std::time::Instant::now() + wait_bound;
    loop {
        let status = { ctx.registry.lock().await.find_item(&item_id).map(|item| item.status) };
        match status {
            Some(choosh_protocol::host_rpc::ItemStatus::Starting) if std::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            Some(other) => {
                println!("WebService {name} readiness: {other:?}");
                return Ok(());
            }
            None => {
                println!("WebService {name} readiness: item no longer registered");
                return Ok(());
            }
        }
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

    #[test]
    fn cli_parses_service_run_matching_service_tunnels_mds_pseudocode() {
        let cli = Cli::parse_from([
            "choosh-hostd",
            "service",
            "run",
            "--workspace",
            "app",
            "--name",
            "web",
            "--port",
            "3000",
            "--protocol",
            "http",
            "--",
            "npm",
            "run",
            "dev",
        ]);
        let Command::Service { command: ServiceCommand::Run { workspace, name, port, protocol, command } } = cli.command else {
            panic!("expected Command::Service(Run), got {cli:?}");
        };
        assert_eq!(workspace, "app");
        assert_eq!(name, "web");
        assert_eq!(port, 3000);
        assert_eq!(protocol, "http");
        assert_eq!(command, vec!["npm".to_string(), "run".to_string(), "dev".to_string()]);
    }

    #[test]
    fn cli_parses_service_run_with_default_protocol() {
        let cli = Cli::parse_from(["choosh-hostd", "service", "run", "--workspace", "app", "--name", "web", "--port", "3000", "--", "cat"]);
        let Command::Service { command: ServiceCommand::Run { protocol, .. } } = cli.command else {
            panic!("expected Command::Service(Run), got {cli:?}");
        };
        assert_eq!(protocol, "http");
    }

    #[test]
    fn cli_parses_update_monitor() {
        let cli = Cli::parse_from([
            "choosh-hostd",
            "update-monitor",
            "--previous",
            "/opt/choosh-hostd.previous",
            "--current",
            "/opt/choosh-hostd",
            "--socket",
            "/run/choosh-hostd/emit.sock",
            "--state-file",
            "/opt/state/update_state.json",
            "--version",
            "0.5.0",
        ]);
        let Command::UpdateMonitor { previous, current, socket, state_file, version } = cli.command else {
            panic!("expected Command::UpdateMonitor, got {cli:?}");
        };
        assert_eq!(previous, std::path::PathBuf::from("/opt/choosh-hostd.previous"));
        assert_eq!(current, std::path::PathBuf::from("/opt/choosh-hostd"));
        assert_eq!(socket, std::path::PathBuf::from("/run/choosh-hostd/emit.sock"));
        assert_eq!(state_file, std::path::PathBuf::from("/opt/state/update_state.json"));
        assert_eq!(version, "0.5.0");
    }

    #[test]
    fn cli_parses_dev_exec_matching_m7s_milestone_doc_shape() {
        let cli = Cli::parse_from(["choosh-hostd", "dev-exec", "--host", "dev-heavy", "--workspace", "app", "--", "cargo", "test"]);
        let Command::DevExec { host, workspace, command } = cli.command else {
            panic!("expected Command::DevExec, got {cli:?}");
        };
        assert_eq!(host, "dev-heavy");
        assert_eq!(workspace, "app");
        assert_eq!(command, vec!["cargo".to_string(), "test".to_string()]);
    }
}
