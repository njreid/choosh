# M0 client-side textual diff audit

Audit date: 2026-07-18. Spike G is partially implemented and is not an exit-gate pass.

Present headless evidence includes a shell-free hardened Git status plan, bounded parsing of
NUL-delimited porcelain records and hostile byte paths, immutable single-use blob capabilities,
pre/post worktree identity checks, stale-byte discard, bounded client-side textual hunks, stable
line navigation, metadata-only binary/encoding/size/work classifications, and exact right-side
reconstruction by applying emitted hunks across additions, deletions, replacements, Unicode, empty
files, and final-newline changes.

The following Spike G evidence remains absent:

- a disposable real-Git hostile repository runner covering hooks, filters, textconv, pager,
  prompts, conflicts, unborn HEAD, mode-only changes, submodules, symlinks, and oversized entries;
- immutable status snapshot IDs and entry IDs composed through daemon RPC;
- resolved HEAD/index object retrieval and root-confined worktree streaming adapters;
- generated/property line-pair coverage with recorded seeds and allocation/time measurement;
- diff cache bounds, eviction, and concurrent request coalescing;
- golden navigation for rename pairs, boundary hunks, deleted historical buffers, and stale current
  document identities;
- one documented non-Android command driving the complete production actor path.

No current test launches repository hooks or external diff helpers, but the fixed command plan alone
does not prove that an actual process adapter preserves its cleared environment and arguments.
