# Android navigation and pinning

Status: Draft

## Workspace entry

The top-level flow is:

```text
Host list → workspace list → workspace
```

Hosts and workspaces are explicit. Selecting a host never scans its filesystem. Selecting a workspace loads a snapshot, subscribes to events, and restores locally stored pins.

## Page model

The explorer is permanently page zero:

```text
[Explorer] [Pinned item] [Pinned item] ...
```

Explorer section order:

1. active agents;
2. registered development services;
3. changed Git files;
4. searchable project tree.

Tapping a row toggles its pinned state. Tapping or gesturing inside an open page never unpins it. Pin order is insertion order in V1. Reordering MAY be added later.

## Pinned kinds

| Kind | Surface |
| --- | --- |
| Agent | Interactive terminal bound to managed Zellij target |
| Service | Isolated WebView through authenticated SSH gateway |
| Markdown | Annotatable internal WebView |
| Source | Sora editor |
| Git diff | Future native unified-diff surface (not implemented in M0) |

Heavy views are retained and rebound to the focused logical page. Focus changes MUST preserve remote processes, Sora revision state, scroll position where practical, and WebView lifecycle policy.

## Search

Explorer search matches cached/streamed root-confined paths. Host-assisted content search is a later capability. Results retain canonical identity and cannot escape the workspace through symlinks or crafted names.

## Notification deep link

An agent notification identifies host, workspace, and item—not terminal text. Activation:

1. select/connect host;
2. open workspace;
3. refresh item snapshot if necessary;
4. pin the agent if absent;
5. focus its terminal page;
6. clear/acknowledge the notification.

If the item no longer exists, Choosh opens the workspace explorer and explains that the request is stale.

## Back behavior

- Within a surface, back first dismisses transient UI/search/selection.
- From a pinned page, back returns to the explorer without unpinning.
- From the explorer, back returns to the workspace list.
- Stopping an agent/service or terminating a workspace always requires a separately labelled destructive action.
