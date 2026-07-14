# M1: Remote workspace vertical slice

## Outcome
A clean Android install connects, registers a workspace, resumes one agent, browses files, and reads Markdown with SSH as the only host listener.

## Requirements
- **M1-R1:** Host profiles use known-host verification and Keystore-backed credentials.
- **M1-R2:** With consent, install/upgrade compatible host binaries and health-check `chooshd`.
- **M1-R3:** List only explicit registrations; canonicalize a root and create/adopt the same-named Zellij session.
- **M1-R4:** Start one agent in a managed tab and provide a complete interactive terminal.
- **M1-R5:** Browse/refresh/filter a root-confined SFTP tree.
- **M1-R6:** Render remote Markdown with bounded, root-confined relative assets.
- **M1-R7:** Reconnect after Android process death without stopping Zellij or the agent.
- **M1-R8:** Keep detach, unregister, agent stop, and session terminate separate.

## Exit gate
From a new install, register `/projects/choosh`, run an agent, kill/reopen Android, resume the TUI, and read README. Traversal and escaping symlinks fail visibly.

## Excluded
Notifications, multiple pins, services, editing, diffs, and annotations.

