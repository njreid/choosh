# Diagnostics and support bundles

## Purpose and defaults

Choosh has no telemetry service and may be sideloaded through Obtainium. It MUST
therefore provide an explicit, local, user-controlled diagnostic export for
support without collecting remote project content by default. Diagnostics are
off by default, retained locally only for a bounded period, and never uploaded
by the application.

## Allowed data

A support bundle MAY contain only versioned, bounded, redacted records such as:

- app, bridge, daemon, and protocol versions; Android API/ABI and renderer
  identity; and an opaque release/build identifier;
- stable error codes, state transitions, reconnect/backoff counters, queue and
  resource-limit counters, and elapsed logical durations;
- declared capability states, without capability tokens or payloads; and
- crash class, sanitized stack frames, and a bounded timestamp.

It MUST NOT contain private keys, credential references, host names, IP
addresses, absolute paths, workspace names, file/document contents, terminal
bytes, clipboard contents, agent prompts/events, opaque tokens, raw Git output,
or unbounded free-form exception messages. An unknown diagnostic field is
rejected rather than exported.

## Export contract

The exporter receives an injected clock, bounded record source, and redactor.
It emits a versioned JSON manifest plus optional bounded text records, all below
a caller-visible maximum size. Export requires an explicit user action each
time; enabling local collection does not grant upload permission. The UI must
show the exact record categories before sharing and permit deletion of the local
buffer.

Crash capture follows the same redaction and bounds. It records no terminal or
editor process memory and does not depend on an external crash-reporting SDK.

## Verification

Headless fixtures inject every forbidden-data canary into paths, errors,
credentials, protocol payloads, terminal text, clipboard text, and exception
messages. They assert that exported bytes contain none of the canaries, that
known fields are canonically ordered, oversized records are rejected or counted
without partial payload output, and no network capability is required. Device
tests verify only the explicit consent, deletion, and Android share-sheet
boundary; screenshots are supplemental evidence, not the oracle.

This contract is a future implementation requirement. No current preview APK
claims to provide diagnostic export or crash capture.
