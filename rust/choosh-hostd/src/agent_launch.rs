//! Builds the fixed argv that launches an agent inside an `AgentTerminal`
//! item's Zellij tab, per `agent-events.md`'s adapter contract
//! (`CHOOSH_WORKSPACE_ID`/`CHOOSH_ITEM_ID`/`CHOOSH_ROOT`/`CHOOSH_AGENT`) and
//! `host-rpc.md`'s "Command construction" (fixed executable + argv, never a
//! shell string).
//!
//! Environment variables can't be set on `zellij action new-tab` itself and
//! reach the spawned process (see `zellij_ops::new_tab`'s doc comment) —
//! this module works around that by prefixing the real `env` utility with
//! explicit `KEY=VALUE` arguments, itself a fixed-argv construction, not
//! shell interpolation.
//!
//! The same `env KEY=VALUE ...` prefix also carries a workspace's
//! Project-pinned `mise env` (`docs/specs/toolchain-provisioning.md`'s
//! first tier, resolved by `mise_ops::project_env` and passed in here by
//! `rpc::handle_item_create`) alongside the fixed `CHOOSH_*` vars — both
//! land in the same `env` invocation, so an agent process sees both without
//! needing two separate wrapping layers.

use choosh_protocol::host_rpc::AgentKind;

/// The real executable name each `AgentKind` launches — also reused as-is
/// for `CHOOSH_AGENT`'s value (`agent_launch_argv`), since both mappings
/// happen to be the same string for every variant today: `codex`/`claude`
/// are confirmed present on `PATH` in this environment; `opencode` is not
/// installed here and its argv construction is untested against a real
/// binary, unlike the other two — a real gap, not silently assumed away.
fn executable(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Codex => "codex",
        AgentKind::Claude => "claude",
        AgentKind::Opencode => "opencode",
    }
}

/// Builds `["env", "CHOOSH_WORKSPACE_ID=...", ..., "K=V", ..., "<executable>"]`
/// — one `"K=V"` entry per `mise_env` pair — ready to pass as
/// `zellij_ops::new_tab`'s `initial_command`. `mise_env` is the workspace's
/// resolved Project-pinned `mise env` (empty for a workspace with no
/// `mise.toml`, per toolchain-provisioning.md — an empty slice adds
/// nothing beyond the fixed `CHOOSH_*` vars this already set).
#[must_use]
pub fn agent_launch_argv(agent: AgentKind, workspace_id: &str, item_id: &str, root: &str, mise_env: &[(String, String)]) -> Vec<String> {
    let mut argv = vec![
        "env".to_string(),
        format!("CHOOSH_WORKSPACE_ID={workspace_id}"),
        format!("CHOOSH_ITEM_ID={item_id}"),
        format!("CHOOSH_ROOT={root}"),
        format!("CHOOSH_AGENT={}", executable(agent)),
    ];
    argv.extend(mise_env.iter().map(|(key, value)| format!("{key}={value}")));
    argv.push(executable(agent).to_string());
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_launch_argv_sets_every_required_env_var_and_ends_with_the_executable() {
        let argv = agent_launch_argv(AgentKind::Claude, "ws-1", "item-1", "/workspaces/app", &[]);
        assert_eq!(
            argv,
            vec![
                "env",
                "CHOOSH_WORKSPACE_ID=ws-1",
                "CHOOSH_ITEM_ID=item-1",
                "CHOOSH_ROOT=/workspaces/app",
                "CHOOSH_AGENT=claude",
                "claude",
            ]
        );
    }

    #[test]
    fn claude_launch_argv_injects_project_pinned_mise_env_between_the_choosh_vars_and_the_executable() {
        let mise_env = vec![("NODE_VERSION".to_string(), "20.20.2".to_string()), ("PATH".to_string(), "/mise/node/20/bin:/usr/bin".to_string())];
        let argv = agent_launch_argv(AgentKind::Claude, "ws-1", "item-1", "/workspaces/app", &mise_env);
        assert_eq!(
            argv,
            vec![
                "env",
                "CHOOSH_WORKSPACE_ID=ws-1",
                "CHOOSH_ITEM_ID=item-1",
                "CHOOSH_ROOT=/workspaces/app",
                "CHOOSH_AGENT=claude",
                "NODE_VERSION=20.20.2",
                "PATH=/mise/node/20/bin:/usr/bin",
                "claude",
            ]
        );
    }

    #[test]
    fn each_agent_kind_maps_to_a_distinct_real_executable_name() {
        assert_eq!(executable(AgentKind::Codex), "codex");
        assert_eq!(executable(AgentKind::Claude), "claude");
        assert_eq!(executable(AgentKind::Opencode), "opencode");
    }
}
