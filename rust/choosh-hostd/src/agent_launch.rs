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

use choosh_protocol::host_rpc::AgentKind;

/// The real executable name each `AgentKind` launches. `codex` and `claude`
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

fn agent_env_value(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Codex => "codex",
        AgentKind::Claude => "claude",
        AgentKind::Opencode => "opencode",
    }
}

/// Builds `["env", "CHOOSH_WORKSPACE_ID=...", ..., "<executable>"]` — ready
/// to pass as `zellij_ops::new_tab`'s `initial_command`.
#[must_use]
pub fn agent_launch_argv(agent: AgentKind, workspace_id: &str, item_id: &str, root: &str) -> Vec<String> {
    vec![
        "env".to_string(),
        format!("CHOOSH_WORKSPACE_ID={workspace_id}"),
        format!("CHOOSH_ITEM_ID={item_id}"),
        format!("CHOOSH_ROOT={root}"),
        format!("CHOOSH_AGENT={}", agent_env_value(agent)),
        executable(agent).to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_launch_argv_sets_every_required_env_var_and_ends_with_the_executable() {
        let argv = agent_launch_argv(AgentKind::Claude, "ws-1", "item-1", "/workspaces/app");
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
    fn each_agent_kind_maps_to_a_distinct_real_executable_name() {
        assert_eq!(executable(AgentKind::Codex), "codex");
        assert_eq!(executable(AgentKind::Claude), "claude");
        assert_eq!(executable(AgentKind::Opencode), "opencode");
    }
}
