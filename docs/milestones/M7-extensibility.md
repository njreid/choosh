# M7 — Versioned extensibility

Status: planned (post-1.0)

M7 defines a deliberately small extension boundary for host adapters and agent
event providers. Extensions are optional capabilities, never a replacement for
the SSH trust boundary or for `chooshd`'s ownership of durable state.

## Compatibility contract

An extension identifies itself with a stable reverse-DNS name and a semantic
API version (`name`, `api`, `capabilities`). Chooshd accepts only versions in
its compiled compatibility range and rejects unknown capabilities before any
side effect. The negotiation response is bounded and contains no paths,
credentials, or executable commands.

The first supported wire family is `adapter.describe-v1`:

```json
{"name":"com.example.adapter","api":"1.0","capabilities":["events"]}
```

Implementations must preserve additive compatibility within major version 1;
an incompatible change requires a new major version and a new adapter name.
Adapters receive narrow capability interfaces through constructor injection and
cannot access process globals, durable stores, or arbitrary host paths.

## Acceptance matrix

The headless gate must prove, deterministically and without network access:

1. a valid `adapter.describe-v1` is accepted and its capabilities are sorted;
2. unsupported major versions, duplicate capabilities, oversized fields, and
   unknown capabilities are rejected before invocation;
3. a v1 adapter remains usable when additive fields are present;
4. adapter output is bounded, schema-valid, and observational (it cannot
   approve, deny, or rewrite an operation);
5. a failed or removed adapter leaves the core event spool and durable state
   unchanged.

Device and live-host work is deferred until this contract has a concrete outer
composition root. No DI framework is introduced for M7; shared contracts stay
framework-agnostic.

