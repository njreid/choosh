# M5 detailed design: Markdown review and annotations

Status: Proposed

This design refines [M5](../milestones/M5-markdown-review.md) within the WebView
boundary selected by [ADR 0003](../adr/0003-android-surfaces.md). Normative terms
describe both production behavior and its headless acceptance harness.

## Outcome and boundary

M5 renders project Markdown as sanitized internal HTML, streams bounded fragments,
serves confined assets with HTTP range semantics, stores resilient local annotations,
and exports them only on an explicit command. Rust owns documents, annotations,
rendering, routing, and persistence. The WebView is an untrusted projection and has
no SSH, SFTP, filesystem, RPC, annotation database, or Java/Kotlin bridge access.

M5 does not provide real-time collaboration, automatic repository writes or commits,
remote URL fetching, proprietary document formats, or arbitrary embedded HTML.

## Components and authority

| Component | Authority | Input trust |
| --- | --- | --- |
| Markdown actor | revisioned source and render generation | source is untrusted |
| Annotation actor | records, anchor status, local persistence, export snapshots | selections/imports are untrusted |
| Loopback router | token validation, route capabilities, ranges, limits | all HTTP input is untrusted |
| Maud renderer | escaped/sanitized HTML fragments | Markdown AST is untrusted |
| Internal WebView | transient selection, focus, accessibility projection | DOM events are untrusted hints |
| Headless harness | scripted route/actor requests and assertions | no alternate render logic |

The internal Markdown origin and development-service origins MUST use distinct
tokens, routing namespaces, and WebView storage profiles where Android supports it.
Only `127.0.0.1` is bound on an ephemeral port. A random port is not authentication.

## Document and rendering model

```text
MarkdownDocument = host ID + workspace ID + canonical document identity
Revision         = monotonically increasing content revision
RenderGeneration = monotonically increasing projection generation
DocumentSnapshot = identity + revision + UTF-8 source digest + source bytes
```

Markdown input uses the same binary, encoding, size, identity, and root-confinement
policy as [the M4 document design](M4-editing-git-diff.md), but preview is read-only.
The initial source ceiling is 2 MiB. A render has explicit ceilings for AST nodes,
nesting depth, table cells, rendered bytes, and wall-clock budget. Exceeding any
ceiling produces an escaped metadata error page, never a partial document presented
as complete.

V1 supports CommonMark plus fenced code, tables, task lists, and relative project
links/assets. Raw HTML is disabled. Code is escaped before optional syntax spans are
added. Generated element and annotation IDs are opaque and deterministic for one
document revision; source text and paths never become HTML IDs.

The response sets a restrictive CSP allowing only the internal origin, bundled
styles, bundled Datastar code, capability-scoped asset URLs, and no remote network,
plugins, frames, forms, inline event handlers, or `javascript:` URLs. WebView file
and content access, mixed content, arbitrary navigation, popups, and JS interfaces
are disabled. External and absolute links are inert metadata in V1. Relative
Markdown links resolve lexically and canonically beneath the workspace root before
they can request navigation.

## Fragment protocol

The initial page is a complete projection for `(document, revision,
render_generation)`. Subsequent server-sent updates contain only versioned internal
fragment operations:

```text
replace_document(new generation, expected previous generation, sanitized HTML)
upsert_annotation(annotation ID, anchor status, sanitized HTML)
remove_annotation(annotation ID)
set_selection(annotation ID or none)
```

Every stream request requires the process token in an HttpOnly, SameSite=Strict
cookie established through a one-use bootstrap capability. Origin and Fetch Metadata
headers are validated where available. Query parameters never carry the process
token. Connections have bounded event size, queue depth, heartbeat rate, and lifetime.
Queue overflow closes the stream and forces a full snapshot reload. A stale or skipped
generation cannot apply incrementally. User strings are rendered only as escaped text.

The WebView reports selection/navigation commands over authenticated loopback HTTP
with document ID, projected revision/generation, and source offsets emitted by the
renderer. Rust validates all fields against its source map; DOM text and client-sent
quotes never establish authority.

## Annotation model

```text
Annotation {
  annotation_id, host_id, workspace_id, document_identity,
  created_revision, last_resolved_revision,
  range_utf8: [start, end), selected_text_digest,
  prefix_context, suffix_context, context_digest,
  body_markdown, status, created_at, updated_at
}

status = attached(range, confidence) | ambiguous(candidate_count) | orphaned(reason)
```

IDs use a local cryptographically random source in production and a seeded source in
tests. Bodies have byte/line limits and render through the same sanitizer. Empty or
invalid UTF-8 ranges, split code points, stale projected revisions, overlapping
protocol fields, and duplicate IDs fail without mutation.

Context stores bounded Unicode scalar windows, normalized to NFC for matching while
retaining exact source offsets and a digest of the exact selected bytes. Context is
source-derived sensitive data: it is encrypted at rest when the application local
store is encrypted, never logged, never placed in URLs, and included in export only
as specified below.

### CRUD and persistence

Create, update, and delete commands include `expected_record_revision`; stale commands
return `stale_revision`. Each successful mutation is one local database transaction,
increments the record revision, and emits an immutable event after commit. Database
keys include stable host identity, workspace ID, and document identity—not display
names or remote absolute paths. Reconnect does not change records. Workspace removal
does not silently delete them; explicit local-data deletion is separate.

Schema versions migrate transactionally. On migration or corruption failure the
store opens read-only, preserves the original database, and permits a diagnostic
export that excludes source context unless explicitly requested.

## Deterministic re-anchoring

Re-anchoring receives the exact old snapshot when retained, the new snapshot, and the
annotation. It performs these ordered steps with fixed byte/operation ceilings:

1. If the exact selected bytes remain at the same range and both bounded contexts
   match, attach there with `exact` confidence.
2. If an old snapshot is available, compute a bounded line/byte edit map. A range not
   touched by an edit maps to its unique new range with `mapped` confidence.
3. Search only the bounded window around the mapped/previous position for candidates
   matching selected-byte digest plus prefix/suffix context. Exactly one best candidate
   attaches with `context` confidence.
4. More than one equally ranked candidate becomes `ambiguous`; zero candidates,
   overlap with rewritten selected text, unavailable required history, or exceeded
   budget becomes `orphaned` with a stable reason.

Tie-breaking never silently chooses by first occurrence. Ambiguous and orphaned
records retain the last attached range only as historical metadata, not as a current
DOM target. Manual reattach is a revisioned update selecting a validated current
range. Re-anchoring is idempotent for identical inputs and processes annotations by
ID so database iteration order cannot change results.

## Asset routes and cache

Markdown assets are referenced by opaque, document-bound route capabilities. Route
creation rejects absolute paths, schemes, encoded separators, NULs, dot traversal,
symlink escape, non-regular files, and paths whose canonical identity changes during
resolution. No response reveals the remote absolute path.

The handler permits `GET` and `HEAD` only and supports one RFC-compatible byte range.
Malformed or multiple ranges return `416`; unsatisfiable ranges return `416` with the
known size. Responses include deterministic `Content-Length`, `Content-Range`,
`Accept-Ranges`, a content-derived validator when available, `nosniff`, and a narrow
allowlist content type determined from validated magic bytes. SVG, HTML, script,
archive, device, socket, and unknown active content are rejected in V1 rather than
served inline.

Before headers, the handler verifies the capability, document binding, canonical
identity, regular-file kind, total-size limit, requested-range limit, and concurrent
stream quota. It rechecks identity after streaming. Cancellation closes the SFTP
reader promptly. Each response is chunked internally and never buffers the entire
asset.

The cache key is immutable remote identity plus byte range. It has process-wide and
per-document byte/entry limits, least-recently-used eviction, no caching of failures,
and request coalescing. Cached entries contain bytes and safe metadata only, never an
SSH credential, capability, or absolute path. Bulk asset reads use independently
bounded SSH channels and yield to control/terminal work.

## Export protocol

Export is always a two-step explicit operation:

1. `annotations.export.prepare` freezes an immutable, bounded snapshot and returns
   format, byte count, digest, destination classification, and a short-lived export ID.
2. `annotations.export.commit` requires that export ID and a user-selected destination.

Supported formats are deterministic Markdown and versioned JSON at
`.choosh/annotations.json`. JSON uses a documented schema version, UTF-8, sorted
records, stable field ordering in fixtures, root-relative document paths, one-based
line/column display positions, bodies, status, and selected/context excerpts bounded
by export policy. It never contains host absolute paths, local database IDs other than
annotation IDs, tokens, credentials, or unbounded source text.

Commit uses the M4 identity-check and atomic-replacement protocol. An existing export
file is not replaced unless its prepared identity still matches. Prepare alone never
writes remotely; reconnect, rendering, annotation CRUD, or agent activity never
triggers export. Partial multi-document export is not reported as success. A generated
artifact is data for any terminal agent, not an instruction to invoke one.

## Stable errors and failure behavior

M5 domain reasons include:

```text
stale_revision, invalid_range, render_limit, unsafe_markdown, unauthorized,
stale_generation, stream_overflow, path_escape, asset_changed, unsupported_media,
range_invalid, range_limit, cache_limit, reanchor_ambiguous, reanchor_orphaned,
store_read_only, export_stale, cancelled
```

Unauthorized requests receive the same minimal response regardless of route
existence. Parser, sanitizer, database, SFTP, and export errors are escaped and mapped
to stable codes. Errors do not echo attacker-controlled HTML, paths, headers, bodies,
or tokens. Retrying reads is safe; annotation mutations require the current record
revision; export commit requires a new preflight after any identity failure.

## Headless verification contract

The repository MUST provide a non-Android executable driver using the production
Markdown, annotation, router, persistence, and export modules. It accepts canonical
newline-delimited JSON commands and can host its loopback HTTP server for ordinary
HTTP clients. A seeded ID source and fake clock remove volatile output.

Fixtures include:

- Markdown source plus golden sanitized HTML/fragment models;
- old/new document pairs plus annotation inputs and golden anchor states;
- a fake SFTP tree with symlinks, identity races, short reads, cancellation barriers,
  MIME-confusion bytes, and deterministic range data;
- a temporary versioned annotation database for restart/migration/corruption tests;
- canonical JSON and Markdown export goldens;
- adversarial HTTP requests generated without a WebView.

Goldens assert semantic HTML using a parsed tree with normalized attribute ordering;
they do not use screenshots or depend on a browser serializer. Accessibility output
is asserted as a semantic tree containing headings, lists, code, links, annotation
status/descriptions, focus order, and actions.

## Acceptance criteria

M5 is complete only when one documented headless command runs all checks without an
emulator, Android UI, external network, or human judgment:

1. CommonMark, fenced code, tables, tasks, Unicode, and malformed input produce the
   expected semantic tree and sanitized HTML within every configured ceiling.
2. Raw HTML, event attributes, scripts, remote URLs, `javascript:` URLs, traversal,
   encoded traversal, symlink escape, and hostile filenames cannot create an active
   route or executable DOM content. CSP and security headers match goldens.
3. Missing/wrong/replayed bootstrap capability, missing cookie, wrong origin, forged
   document ID, and development-service token all fail identically without revealing
   route existence.
4. Fragment generations apply in order; skipped/stale generations and queue overflow
   force a full reload. Annotation body/source strings remain escaped in every event.
5. CRUD survives database close/reopen and simulated reconnect. Stale concurrent
   updates have exactly one winner; transaction failure publishes no event or partial
   record.
6. Non-overlapping insert/delete fixtures retain anchors. Repeated identical text is
   ambiguous, overlapping rewrites orphan, missing history fails safely, and manual
   reattach validates the current revision.
7. Re-anchoring property tests are idempotent, never return an out-of-range/split-code-
   point anchor, never silently select among equal candidates, and remain within the
   operation budget. Failure output records the reproducible seed.
8. `GET`, `HEAD`, first/last/open-ended/suffix/unsatisfiable/multiple ranges, validator
   changes, short reads, cancellation, identity races, cache eviction, and coalesced
   requests produce exact status/header/body results within byte/concurrency limits.
9. Magic-byte/type confusion, active SVG/HTML, oversized assets, special files, and
   changing files fail closed; logs and responses contain no absolute path or token.
10. Prepare has no filesystem side effect. Explicit commits produce schema-valid,
    byte-for-byte deterministic JSON/Markdown exports; stale destinations and injected
    write/rename failures never yield a truncated successful export.
11. The semantic accessibility tree exposes annotations and attached/ambiguous/orphaned
    state with deterministic focus/navigation actions usable without touch.
12. Fuzz targets for Markdown parsing, route parsing, range parsing, annotation command
    decoding, and re-anchoring complete the CI smoke corpus without panic or unbounded
    allocation.

## Traceability

| Milestone requirement | Design sections |
| --- | --- |
| M5-R1–R2 | Document/rendering model, fragment protocol |
| M5-R3–R5 | Annotation model, persistence, re-anchoring |
| M5-R6 | Export protocol |
| M5-R7 | Asset routes and cache |
| M5-R8 | Headless verification and acceptance criteria |
