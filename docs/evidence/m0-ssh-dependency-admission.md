# M0 SSH dependency admission experiment

Status: planned; this is a dependency-free contract. It does not approve, add, or
implicitly select an SSH implementation.

This experiment is the admission gate for the adapter behind the shared
[M0 SSH acceptance harness](m0-ssh-acceptance-harness.md). It supplements the
candidate research in [the transport choice record](m0-ssh-transport-choice.md)
with reproducible inputs, negative assertions, and machine-readable evidence.
An adapter-specific happy-path test, benchmark, or package manifest is not an
alternative to this gate.

## Candidate declaration

Before resolving a candidate, the spike commit MUST add a reviewed declaration
under `fixtures/ssh-admission/<candidate>/candidate.json` conforming to a new
draft 2020-12 schema. The declaration is public metadata and MUST contain:

- package names, exact versions, enabled and disabled features, and the selected
  cryptographic backend;
- the expected `Cargo.lock` digest and the complete resolved-package list;
- each native component, source origin and digest, Android build path, and ABI it
  contributes, including transitive components;
- licence SPDX expression, notice/source-offer obligations, and the repository
  evidence path that records their disposition;
- the fixture server implementation and exact version, test-key generation
  method, and the supported host-key and user-key algorithms;
- an explicit `pre_release_exception` object, or `null`. A non-null exception
  MUST cite an approved ADR, owner, expiry date, removal condition, and the exact
  prerelease packages it permits.

The declaration MUST NOT contain private keys, credentials, real endpoints,
absolute paths, generated public-key bytes, or a development machine identity.
The fixture generator creates keys under its temporary directory at runtime and
reports only an algorithm label plus a SHA-256 fingerprint.

## Admission checks

The candidate is admissible only when all checks below pass from a clean checkout.
The spike MUST fail closed if any check is absent, skipped, or produces an
unclassified result.

| Check | Deterministic assertion | Evidence retained |
| --- | --- | --- |
| Lock and release policy | `cargo metadata --locked` resolves exactly the declaration; no production prerelease appears unless its unexpired exception names it; no dynamic version or unreviewed feature is present. | canonical resolved-package JSON and lock digest |
| Legal/native inventory | Every crate and native transitive has an approved licence disposition and required notice/source-offer material; every native source and build recipe is recorded. | SPDX/CycloneDX inventory and policy result |
| Android build | The selected Rust toolchain builds the adapter and its native closure for `aarch64-linux-android` and `x86_64-linux-android`; packaged APKs contain only approved native libraries for those ABIs. | toolchain versions, APK library listing, ABI checksums |
| Host-key ordering | Unknown, rejected, and changed keys invoke no authentication method and open no SSH channel. Exact stored key is the only path that may invoke authentication. | canonical event trace and fixture assertions |
| Authentication isolation | Invalid or unavailable credentials return a typed failure after exact-key verification, without key bytes, credential references, passphrases, paths, or payloads in logs/evidence. | redacted canonical event trace and log-scrub result |
| Channel surface | One verified connection exercises PTY, fixed-argument exec, root-confined SFTP, and registered-loopback `direct-tcpip`; unsupported operations fail typed rather than silently emulating a second connection. | channel/generation counters and capability result |
| Bounds and fairness | The shared throttled-SFTP scenario meets queue, deadline, cancellation, disconnect, and p99 PTY-progress assertions with injected logical time only. | canonical fixture result and counter summary |

The spike MUST run its package-resolution and legal checks before networked fixture
work. A candidate that cannot satisfy the lock, legal, or Android build checks is
rejected without an adapter implementation merge.

## Host-key-before-auth fixture protocol

The runner records one ordered event stream. Event names are stable and values are
limited to fixture IDs, algorithm labels, fingerprints, generation numbers, channel
kinds, and typed outcomes. It records neither secrets nor raw packets.

For each case, the fixture asserts the stated prefix and prohibited events:

| Fixture ID | Required ordered events | Prohibited before final outcome |
| --- | --- | --- |
| `unknown_key` | `transport_connected`, `host_key_presented`, `trust_consent_required`, `transport_closed` | `authentication_started`, `channel_opened` |
| `unknown_key_rejected` | `transport_connected`, `host_key_presented`, `trust_consent_required`, `trust_rejected`, `transport_closed` | `authentication_started`, `channel_opened` |
| `unknown_key_accepted_exact` | `transport_connected`, `host_key_presented`, `trust_consent_required`, `trust_accepted_exact`, `authentication_started`, `authentication_succeeded`, `ready` | `channel_opened` before `ready` |
| `stored_key_exact` | `transport_connected`, `host_key_presented`, `host_key_matched`, `authentication_started`, `authentication_succeeded`, `ready` | `trust_consent_required`, `channel_opened` before `ready` |
| `stored_key_changed` | `transport_connected`, `host_key_presented`, `host_key_mismatch`, `transport_closed` | `trust_consent_required`, `authentication_started`, `channel_opened` |
| `stored_key_exact_bad_credential` | `transport_connected`, `host_key_presented`, `host_key_matched`, `authentication_started`, `authentication_failed`, `transport_closed` | `ready`, `channel_opened` |

`trust_accepted_exact` carries the pending fixture fingerprint and a consent token
bound to the fixture profile ID and endpoint ID. The runner retries each scenario
with a stale fingerprint, a different profile ID, and a different endpoint ID; each
must fail as `trust_consent_invalid` without `authentication_started`. Authentication
instrumentation is injected at the adapter seam, so an implementation cannot pass by
opening a channel or attempting a credential exchange before the callback is counted.

## Deterministic fixture shape

The schema and fixtures introduced with the real spike MUST be versioned. A minimal
fixture has this logical shape:

```json
{
  "fixture_version": 1,
  "id": "stored_key_changed",
  "seed": "ssh-admission-v1",
  "endpoint_id": "fixture-endpoint-a",
  "profile_id": "fixture-profile-a",
  "stored_fingerprint": "SHA256:fixture-key-a",
  "presented_fingerprint": "SHA256:fixture-key-b",
  "credential": "fixture-credential-invalid",
  "limits": { "channels": 4, "per_channel_bytes": 65536, "aggregate_bytes": 262144 },
  "steps": []
}
```

The values above are fixture identifiers, not cryptographic material. The schema
MUST constrain IDs, nonzero limits, supported algorithms, and step ordering. Tests
MUST reject unrecognized fields, duplicate step ordinals, decreasing logical time,
and fixture data that resembles an absolute path or a private-key block.

All scheduling uses the logical clock defined by the harness. The fixture server may
use a real socket only in the adapter lane, but it receives deterministic fault
instructions and has a bounded test timeout solely as a deadlock guard. Assertions
use event order, logical duration, counters, and byte bounds; they MUST NOT depend on
wall-clock performance, port numbers, thread timing, external DNS, or an installed
SSH daemon.

## Commands and promotion record

Command names are reserved until their implementing packages exist:

```sh
cargo test -p choosh-ssh-harness --test dependency_admission
./scripts/check-android.sh
```

The first command executes the scripted lane and, when a candidate is declared, the
disposable local-server adapter lane. The second verifies the Android packaging
closure. CI writes one canonical JSON result per fixture plus an admission summary
containing declaration digest, lock digest, toolchain versions, ABI results, licence
policy result, and redacted event counters. Golden updates require an explicit,
reviewed command; tests never rewrite results.

Promotion requires a reviewer to compare the candidate declaration, lock diff,
licence/native inventory, and canonical results. Passing the experiment authorizes
only the declared adapter graph. Any package, feature, crypto backend, native
component, fixture-server version, or exception change reruns admission. A later
runtime integration still must satisfy the M0 acceptance and release gates.
