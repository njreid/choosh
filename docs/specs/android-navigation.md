# Android navigation and pinning

Status: Draft

## Fleet drawer

Before a Workspace is open, a left-drawer is how the whole fleet is
navigated. It has three switchable sort modes, chosen via three icons
pinned at the top of the drawer:

1. **Project** (default) — `Project → DevHost → Workspace`. Every
   registered Project, each expandable to the DevHosts holding at least one
   of its Workspaces, each expandable to those Workspaces.
2. **Host** — `DevHost → Workspace`, scoped to Projects with current
   activity (a Project counts as active if any of its Workspaces has a
   connected agent/service Item or an event within a recent window — the
   exact staleness bound is an implementation choice, not a wire
   contract). Every DevHost from `list-devhosts` (see
   [relay-protocol.md](relay-protocol.md)) still appears even if it
   currently owns no active Workspace, so the fleet's online/offline state
   stays visible in this mode too.
3. **Recent** — a flat list of every Workspace across the whole fleet,
   most-recently-active first, no grouping.

**Attention flagging is a row property, not a fourth mode.** In every sort
mode, any row whose subtree contains a Workspace with an outstanding
(unacknowledged) `input_required` (see [agent-events.md](agent-events.md))
MUST render a distinct visual marker, propagated up through Project/DevHost
group rows so it's visible without expanding them. Switching sort order
MUST NOT cause an attention-needing Workspace to become harder to find.

**Tapping a Project row opens its designated primary Workspace directly**,
skipping an intermediate Workspace list. A Project's primary Workspace is
explicit — it defaults to the first Workspace registered for that Project
and is changeable afterward (an update to the Project record via
[host-rpc.md](host-rpc.md); this document doesn't repeat that RPC's shape).
Tapping a DevHost row or a Workspace row (in Host or Recent mode) behaves
as the flow below.

**Not yet implemented**: `host-rpc.md`'s `project.list`/
`project.set_primary_workspace` RPCs have no wire type, host handler, or
Android call site today — the Fleet drawer's Project sort mode currently
renders from static fixture data (`FleetFixtures.projectsFor`), not a live
RPC. See [PLAN.md](../../PLAN.md)'s Known follow-ups.

## Workspace entry

Selecting a DevHost or a specific Workspace row is:

```text
Fleet drawer → Workspace list → Workspace
```

DevHosts and Workspaces are explicit. Selecting a DevHost never scans its
filesystem; its Workspace list comes from `workspace.list` (see
[jj-integration.md](jj-integration.md)). Selecting a Workspace loads a
snapshot, subscribes to its event stream, and restores locally stored pins.

## Page model

The explorer is permanently page zero:

```text
[Explorer] [Pinned item] [Pinned item] ...
```

Explorer section order:

1. active agents;
2. registered development services;
3. changed files (`workspace.status`, see [jj-integration.md](jj-integration.md));
4. searchable project tree.

Tapping a row toggles its pinned state. Tapping or gesturing inside an open
page never unpins it. Pin order is insertion order in V1. Reordering MAY be
added later.

## Pinned kinds

| Kind | Surface | Backed by |
| --- | --- | --- |
| `AgentTerminal` | Interactive terminal bound to a managed Zellij target | A relay tunnel to the workspace's Zellij pane — see [terminal-experience.md](terminal-experience.md) |
| `JjChangeGraph` | Interactive commit/operation DAG, tap-to-inspect, one-tap undo | `workspace.log` / `workspace.op.log` / `workspace.op.undo` — see [jj-integration.md](jj-integration.md) |
| `JjDiff` | Native unified diff of two revisions | `workspace.diff` — see [jj-integration.md](jj-integration.md) |
| `SourceEditor` | Sora editor | [editor-protocol.md](editor-protocol.md) |
| `MarkdownPreview` | Annotatable internal WebView | `choosh-web` Maud/Datastar rendering, tunneled the same way as `WebService` |
| `WebService` | Isolated WebView through a relay-tunneled loopback port | `open-tunnel` against a registered service port — see [relay-protocol.md](relay-protocol.md) |
| `EditorPresence` | Read-only "editing in Zed on `<host>`" indicator | `editor_attached` event — see [agent-events.md](agent-events.md) |

Android MUST NOT compute a diff, a change graph, or file-content hashing
on-device: `JjChangeGraph` and `JjDiff` render only structured data `hostd`
already computed via `jj-lib`. There is no client-side diff engine in this
architecture — this is a deliberate difference from the superseded
Git-based design, not an oversight.

Heavy views are retained and rebound to the focused logical page: only one
terminal renderer, one document WebView, one service WebView, and one Sora
`CodeEditor` stay mounted at a time. Focus changes MUST preserve remote
processes, Sora revision state, scroll position where practical, and
WebView lifecycle policy.

## Search

Explorer search matches cached/streamed root-confined paths from
`workspace.tree.list`. Host-assisted content search is a later capability.
Results retain canonical identity and cannot escape the workspace through
symlinks or crafted names.

## Notification deep link

An agent notification (delivered via FCM when backgrounded — see
[notifications.md](notifications.md)) identifies DevHost, Workspace, and
Item — never terminal text, command text, or file contents. Activation:

1. connect to `relayd` if not already connected;
2. resolve the DevHost and open its Workspace;
3. refresh the Item snapshot if necessary;
4. pin the `AgentTerminal` if absent;
5. focus its terminal page and open a relay tunnel to it;
6. clear/acknowledge the notification.

If the Item no longer exists, Choosh opens the Workspace explorer and
explains that the request is stale.

## Back behavior

- Within a surface, back first dismisses transient UI/search/selection.
- From a pinned page, back returns to the explorer without unpinning.
- From the explorer, back returns to the Workspace list.
- Stopping an agent/service or terminating a Workspace always requires a
  separately labelled destructive action.
