# `relayd` threat model

Status: reviewed, M8 scope. Covers `docs/milestones/M8-security-and-release.md`'s
threat-model exit criterion and DESIGN.md §11 ("Security model"). This is a
review of the current `rust/choosh-relayd` implementation as it stands, not
a restatement of design intent — every claim below is traced to a specific
function, check, or test in `ws.rs`, `state.rs`, `ca.rs`, or `webauthn.rs`.

**Additive, not a replacement.** This document covers `relayd`-specific
abuse cases only: identity, enrollment, credential compromise, tunnel
isolation, and availability. It does not re-litigate the path/redaction/
command-construction threat model already established elsewhere in this
project — root-confinement in `choosh-hostd`'s `fs_ops.rs`, fixed-argv
command construction throughout `choosh-hostd`, and agent-event redaction
per `docs/specs/agent-events.md`. Those hold regardless of `relayd`'s own
posture and are unchanged by this review.

## Scope and method

Read in full for this review: `rust/choosh-relayd/src/ws.rs`, `state.rs`,
`ca.rs`, `webauthn.rs`, `docs/specs/relay-protocol.md`,
`docs/specs/auth-and-enrollment.md`, `docs/specs/ssh-bridge-and-zed.md`.
Three issues found during this review were fixed as part of it (a real
frame-size-enforcement bug, a defense-in-depth hardening, and a stale spec
table — all noted inline below and summarized at the end); everything else
is reported as-is, including gaps that are real but too large to fix in
this pass.

---

## 1. Devhost/laptop identity impersonation

**Attack.** A connection claims to be device X (e.g. to receive X's tunnels,
or to appear as a trusted devhost in `list-devhosts`) without actually
holding X's private key.

**Current code behavior.** `authenticate_device` (`ws.rs:157-208`) is the
only path that produces an `Authenticated { device_id, identity_class }` for
a machine Identity. The sequence:

1. `ca::verify` (`ca.rs:115-143`) checks the CA's Ed25519 signature over the
   presented certificate body and returns `(cert_device_id, cert_public_key)`
   — both taken *from the certificate*, which only `relayd`'s own
   enrollment CA key can have signed (`ca.rs:84-103`, `ca::issue`).
2. `ws.rs:165-167` rejects outright if the caller's presented
   `device_auth.device_id` doesn't match `cert_device_id`.
3. `ws.rs:169-172` looks up the registry entry for that device and requires
   it to exist and not be revoked (`entry.revoked`, `ws.rs:173-175`).
4. `ws.rs:176-178` cross-checks the certificate's bound public key against
   the enrollment record's stored public key (defense against a cert whose
   key doesn't match what was recorded at enrollment).
5. `ws.rs:180-197` verifies an Ed25519 signature, made with that same
   public key, over the fresh per-connection nonce `relayd` generated in
   `send_hello` (`ws.rs:84-100`) — proof of private-key possession on
   *this* connection, not just at enrollment. The nonce is freshly random
   (`crate::rng::os_rng()`, backed by the OS CSPRNG — `rng.rs`) per
   connection, so a captured signature cannot be replayed on a new
   connection.

Only after all five checks pass does `Authenticated.device_id` get set. As
of this review, that field is populated from `cert_device_id` (the
CA-verified value), not `device_auth.device_id` (the caller's raw claim) —
**this was changed as part of this review** (`ws.rs:199-207`): previously it
was set from `device_auth.device_id.clone()`, which was provably equal to
`cert_device_id` only because of the explicit equality check at
`ws.rs:165-167`, one line above. Functionally identical today, but bound
directly to the verified source now, so a future edit that weakened or
dropped that equality check couldn't silently reintroduce a
claim-not-derived-from-the-certificate identity bug. Covered by the existing
`devhost_with_bad_signature_is_rejected_and_connection_closes` integration
test plus the full `authenticate_device` test suite; `cargo test -p
choosh-relayd` passes with the change.

`device_id` is never taken from an unverified claim anywhere in the
authenticated path: `identity_class` similarly comes from the registry
entry (`entry.identity_class`, set once at `handle_enroll`, `ws.rs:781-863`),
never from a client-supplied field post-enrollment.

**Verdict: Mitigated.**

**Reasoning.** Impersonation requires either forging an Ed25519 signature
(the CA's, over the certificate; or the device's own, over the connection
nonce) or stealing the actual private key material, neither of which this
control-plane logic can prevent — that's a key-custody problem, addressed in
Case 3. Within what code review can verify: there is no path where a
`device_id` reaches `Authenticated` without both a CA signature and a
fresh possession-proof signature checking out.

---

## 2. Enrollment-token replay or theft

**Attack.** A token is intercepted (shell history, screen share, a log
line) and used by someone other than the intended device, or used twice.

**Current code behavior.** `consume_token` (`ws.rs:875-891`) is the only
path that accepts a token:

```
let Some(entry) = tokens.get_mut(token) else { return Err("token_unknown") };
if entry.consumed { return Err("token_consumed"); }
if now_unix() > entry.expires_at_unix { return Err("token_expired"); }
if entry.identity_class != requested_class { return Err("token_wrong_class"); }
entry.consumed = true;
Ok(())
```

This is a real state transition, not just a TTL: `entry.consumed = true` is
set under the same `registry.tokens.write()` lock that the existence/
consumed/expiry checks ran under, so there's no TOCTOU window for two
concurrent `enroll` calls with the same token to both succeed — the second
one sees `entry.consumed == true` and fails closed with `token_consumed`.
Verified directly by `integration_tests::enroll_token_is_single_use`, which
replays the identical token twice and asserts the first succeeds and the
second doesn't.

Expiry window: `ENROLLMENT_TOKEN_VALIDITY_SECONDS = 15 * 60` (`state.rs:134`),
matching auth-and-enrollment.md's documented 15 minutes. Checked in
`consume_token` against `now_unix()`, and covered by
`enroll_with_unknown_or_expired_token_fails`.

Tokens are minted only from `RequestEnrollmentToken`
(`ws.rs:549` `dispatch`, case `ControlRequest::RequestEnrollmentToken`),
gated to `authenticated.identity_class != IdentityClass::Phone` returning
`not_permitted` — i.e. issuable only from an authenticated phone connection,
matching auth-and-enrollment.md. There is no unauthenticated token-issuance
path.

**If a token leaks in transit or in logs before use:** the blast radius is
bounded to "one `enroll` exchange, within 15 minutes, for one identity
class" — a thief who captures the token before the legitimate device uses
it can enroll *their own* device as a trusted Identity in the legitimate
device's place (the token doesn't bind to any device fingerprint beyond the
`identity_class` it was scoped to), but cannot re-enroll a second time once
either party consumes it, and cannot do anything with it after 15 minutes.
Token values themselves are generated via `generate_token` (`ws.rs:895-899`),
24 bytes of OS CSPRNG entropy — not derivable or guessable.

**Verdict: Mitigated for replay; accepted risk for pre-use theft.**

**Reasoning.** Single-use is a genuine, race-free state transition, not a
documentation-only claim — confirmed by direct code reading and the
existing test. Pre-use interception racing the legitimate device to consume
the token first is an accepted risk inherent to any bearer-token enrollment
scheme: the 15-minute window and single-use property together bound it to
"a narrow race for one unauthorized enrollment," and auth-and-enrollment.md's
revocation section covers the cleanup path (revoke the wrongly-enrolled
device) — though see Case 3 below for the gap in that revocation path
itself.

---

## 3. A compromised laptop-proxy or devhost device credential

**Attack.** An attacker obtains a laptop-proxy's or devhost's private key
and certificate (e.g. from a stolen laptop's keystore, or a devhost's
filesystem) and connects as that Identity.

**Current code behavior — capability scope.** `check_open_tunnel_permitted`
(`ws.rs:516-534`) and the per-request checks inside `dispatch`
(`ws.rs:549-655`) are where auth-and-enrollment.md's capability table is
actually enforced:

- `laptop-proxy`: `open-tunnel` only with `purpose == "ssh"`
  (`ws.rs:516-524`, the `IdentityClass::LaptopProxy if purpose == "ssh"`
  arm), and `ListDevhostSshEndpoints` (`ws.rs:598-602`, restricted to
  `IdentityClass::LaptopProxy`). Every other `dispatch` arm
  (`RequestEnrollmentToken`, `ListDevhosts`, `AgentEvent`,
  `RegisterFcmToken`) explicitly rejects a non-matching identity class with
  `not_permitted`.
- `devhost`: `open-tunnel` only with `purpose == "offload"`
  (`ws.rs:516-524`), `AgentEvent` (`ws.rs:635-641`), and accepts inbound
  tunnels (no explicit "accept" call needed — a devhost that's the
  `target_device_id` of someone else's `open-tunnel` just starts receiving
  `0x02` frames for that tunnel, per `handle_tunnel_frame`).

**Capability-table drift found and fixed during this review:**
`auth-and-enrollment.md`'s table stated laptop-proxy may do
`open-tunnel`-ssh-only, "nothing else" — but the code (and
`ssh-bridge-and-zed.md`, which documents `proxy sync`'s need for it) also
grants laptop-proxy `list-devhost-ssh-endpoints`, a restricted read of
alias + SSH host key per devhost. The table was stale relative to the
actual M6 implementation. **Fixed**: `docs/specs/auth-and-enrollment.md`'s
laptop-proxy row now lists this capability explicitly. No code changed —
the code was already correct; only the spec was out of sync with its own
M6 addition. The `phone` and `devhost` rows were checked the same way and
match the code exactly (no further drift found).

**What a stolen credential can actually do**, given the above:

- A stolen **laptop-proxy** credential can open `ssh`-purpose tunnels to
  any non-revoked devhost (attacker gets a raw SSH byte pipe into every
  devhost's loopback SSH server — a full blast radius across the fleet, by
  design: this is the same trust laptop-proxy already carries for its owner)
  and can read every devhost's alias + SSH host public key. It cannot list
  devhost presence/connection state, request enrollment tokens, or receive
  agent events.
- A stolen **devhost** credential can send `agent-event`s attributed to that
  device (informational — `relayd` just routes them, doesn't act on
  content) and open `offload`-purpose tunnels to other devhosts. It cannot
  reach the phone or another laptop-proxy's tunnels, list devhosts, or
  request enrollment tokens.

**Revocation — real gap found, not fixed.** Both `authenticate_device`
(`ws.rs:173-175`) and `check_open_tunnel_permitted`
(`ws.rs:527-533`, filtering `!device.revoked`) correctly *honor*
`EnrolledDevice.revoked` wherever it's checked — a revoked device's next
connection attempt does fail closed, exactly as auth-and-enrollment.md
promises. But there is **no code path anywhere in `choosh-relayd` that ever
sets `revoked = true`.** Grepping the crate for any write to that field, or
any `ControlRequest`/HTTP route resembling a revoke operation, finds none —
`EnrolledDevice::revoked` is initialized `false` at `handle_enroll`
(`ws.rs:858`) and never mutated again. The same is true for phone passkey
credential revocation (auth-and-enrollment.md's "a revoked phone passkey
credential invalidates that credential's stored session token immediately")
— there is no revoke endpoint for `phone_sessions` either.

This is **not a small, safe fix**: a real revoke path needs a
passkey-gated, phone-only control frame or HTTP route (per
auth-and-enrollment.md, "an operator-initiated revoke... itself
passkey-gated"), and — since `Registry` has no disk persistence at all yet
(`state.rs:1-7`'s crate doc already flags this as a known M0 gap) — a
decision about whether revocation needs to survive a `relayd` restart before
it's trustworthy as an incident-response tool. Implementing it well is a
scoped follow-up, not a patch.

**Verdict: Capability scoping is mitigated (and now correctly documented).
Revocation is an accepted risk — named as an explicit follow-up, not a
silent gap.**

**Reasoning.** The capability checks are real, enforced in code, covered by
tests (`laptop_proxy_may_only_open_ssh_purpose_tunnels`,
`non_phone_identity_cannot_call_list_devhosts_or_request_enrollment_token`,
`register_fcm_token_from_a_devhost_is_rejected`, others), and — after this
review's fix — match the documented table. But "what can a stolen
credential do" is only half the risk model; "how fast can you shut it off"
is the other half, and today the honest answer is: **not via any exposed
mechanism** — the only way to invalidate a compromised device credential
right now is to restart `relayd` with a hand-edited state directory, which
isn't a real operational path. This should be tracked as a named follow-up
before this system is trusted with anything sensitive; M8's exit criterion
("a documented mitigation or an accepted risk for each") is satisfied by
this paragraph being explicit about it, not by pretending the gap doesn't
exist.

---

## 4. Tunnel cross-wiring (Identity A reaching a tunnel meant for Identity B)

**Attack.** Frames intended for one tunnel (and thus one pair of Identities)
are delivered to, or accepted from, the wrong connection.

**Current code behavior.**

- **Tunnel ID generation.** `generate_tunnel_id` (`ws.rs:505-509`) draws
  `TUNNEL_ID_BYTES` (8, `choosh_protocol::relay`) bytes from the OS CSPRNG —
  64 bits of entropy, not derived from any predictable counter or
  timestamp. Not guessable in practice.
- **Collision handling.** `OpenTunnel`'s handler (`ws.rs:620-629`) calls
  `state.registry.tunnels.write().await.insert(tunnel_id, Tunnel {...})`
  with no existence check first — a colliding ID would silently overwrite
  an existing tunnel entry. This is a real gap in the strict sense (no
  explicit collision guard), but with 64 bits of random ID space and a
  single-tenant relay realistically holding, at most, low hundreds of
  concurrent tunnels, the birthday-bound collision probability is
  astronomically below any practical concern (over 2^32 concurrent tunnels
  needed for even a 50% collision chance) — not worth adding a check for.
- **Routing.** `handle_tunnel_frame` (`ws.rs:377-408`) is the only place a
  `0x02` frame gets routed. It looks up the tunnel by ID
  (`state.registry.tunnels.read().await.get(&tunnel_id)`), then calls
  `tunnel.other_party(from_device_id)` (`state.rs:70-83`) — which returns
  `Some` **only** if `from_device_id` equals either
  `tunnel.requester_device_id` or `tunnel.target_device_id`, and `None`
  (frame silently dropped, `ws.rs:382-384`) otherwise. A third Identity that
  somehow learned a tunnel ID (e.g. by observing wire bytes on its own,
  unrelated connection — it can't, since IDs aren't broadcast anywhere
  except to the two actual parties via `OpenTunnelOk` and
  `TunnelOffered`) could not inject frames into it even if it tried,
  because `from_device_id` is the server's own record of which
  *authenticated connection* the frame arrived on — it's threaded into
  `handle_tunnel_frame` from `&authenticated.device_id` at the call site in
  `serve_authenticated_loop` (`ws.rs:313`), never read from the frame's own
  bytes. There is no `device_id` field in the `0x02` frame format at all
  (`decode_tunnel_frame` in `choosh_protocol::relay` strips only class byte
  + tunnel ID + payload) — cross-wiring via a forged sender claim is
  structurally impossible, not just checked-for.
- **Lifecycle discipline.** A tunnel ID is removed from the registry on
  close (explicit zero-payload frame, `ws.rs:386-392`; idle timeout,
  `reap_idle_tunnels`, `ws.rs:482-503`; or either party's disconnect,
  `close_tunnels_for_device`, `ws.rs:457-477`) and never reused for a new
  tunnel — a fresh `open-tunnel` always calls `generate_tunnel_id` again,
  so there's no window where an old ID silently starts referring to a new
  pair of Identities.

**Verdict: Mitigated.**

**Reasoning.** The routing decision is anchored to the server's own
knowledge of which live, authenticated connection a frame arrived on, not
to anything inside the frame — this is the strongest possible binding
short of per-frame cryptographic tagging, and per-frame tagging would be
redundant given the connection is already itself authenticated end to end
(Case 1). The one theoretical gap (no explicit collision guard on
`tunnel_id` insert) is real but not exploitable at any realistic scale for
a single-tenant relay, so it's noted rather than treated as a live risk.
Covered by `phone_opens_tunnel_to_online_devhost_and_bytes_route_both_ways`,
`zero_payload_frame_closes_tunnel_and_further_frames_are_dropped`, and
`disconnecting_one_party_mid_tunnel_closes_it_for_the_survivor`.

---

## 5. `relayd` availability/DoS

**Attack.** Connection floods, oversized frames, slow-loris partial frames,
or unbounded per-connection state exhaust `relayd`'s memory, CPU, or
bandwidth against the single always-on instance the whole fleet depends on.

**Current code behavior — frame bounds.**

- **Control frames:** `MAX_CONTROL_FRAME_BYTES` (1 MiB, defined in
  `choosh_protocol::relay`). Enforced on ingress via `FrameDecoder`
  (`framing.rs:105-155`): a length prefix over the configured limit fails
  the frame *before* any payload bytes are buffered
  (`framing.rs:127-129`, checked right after the 4-byte header is read) —
  so an attacker cannot force a large allocation just by claiming a large
  length; the decoder rejects on the header alone.
- **Tunnel frames — bug found and fixed during this review.**
  `MAX_TUNNEL_FRAME_BYTES` (256 KiB, also in `choosh_protocol::relay`) is
  relay-protocol.md's documented cap, but the single `FrameDecoder`
  instance used for a connection's entire lifetime (`new_decoder()`,
  `wire.rs:17-19`) is sized to `control_frame_limits()` —
  `MAX_CONTROL_FRAME_BYTES`, the *larger* control-frame cap — and that same
  decoder handles both `0x01` control frames and `0x02` tunnel frames in
  `serve_authenticated_loop` (`ws.rs:264-334`). Before this review's fix, a
  tunnel frame between 256 KiB and 1 MiB would pass ingress decoding
  uncaught: it would only fail later, inside `handle_tunnel_frame`'s
  attempt to re-encode it for forwarding
  (`wire_tunnel_frame`, `ws.rs:433-439`, which *does* use
  `MAX_TUNNEL_FRAME_BYTES`) — silently tearing the tunnel down rather than
  terminating the connection, which is what relay-protocol.md's framing
  section actually requires ("a frame exceeding its class's size cap...
  `relayd` MUST terminate the connection"). **Fixed**: an explicit size
  check was added in the `FRAME_CLASS_TUNNEL` arm of
  `serve_authenticated_loop` (`ws.rs:293-314`, the size comparison itself
  at `ws.rs:307-312`) that closes the connection outright if a decoded
  tunnel frame exceeds `1 + TUNNEL_ID_BYTES + MAX_TUNNEL_FRAME_BYTES`.
  Covered by a new regression test,
  `oversized_tunnel_frame_closes_the_connection` (`integration_tests.rs`),
  which sends a 256 KiB + 1 byte tunnel payload framed under the *control*-
  frame cap (exactly the previously-uncaught window) and asserts the
  connection closes. `cargo test -p choosh-relayd` passes (30/30) and
  `cargo clippy -p choosh-relayd --all-targets -- -D warnings` is clean
  with this change in place.
- **Slow-loris / partial frames:** `FrameDecoder::feed` (`framing.rs:105-155`)
  is fully incremental — it accepts arbitrary byte fragments across
  multiple calls and only ever buffers up to `expected_payload` bytes for
  the frame currently in flight (bounded by the size checks above), so a
  connection that trickles bytes one at a time cannot grow `relayd`'s
  per-connection buffer beyond one frame's worth. There is, however, no
  *time* bound on how long a partial frame may sit open — a connection that
  sends 3 of 4 header bytes and then never sends the 4th holds that
  allocation (small, since it's pre-header) indefinitely with no idle
  timeout at the framing layer. `TUNNEL_IDLE_TIMEOUT_SECONDS = 300`
  (`state.rs:97`) only bounds *tunnels with no data frames*, not raw
  WebSocket connections stalled mid-frame — a connection that authenticates
  and then sends nothing further (not even a truncated frame) is never
  reaped. This is a real, unaddressed gap.
- **Per-connection outbound backpressure:** `OUTBOUND_CHANNEL_CAPACITY = 256`
  (`state.rs:93`) bounds each connection's outbound queue; `forward_data`
  (`ws.rs:414-424`) uses non-blocking `try_send` and closes the tunnel
  (rather than blocking or growing the queue) when a target's queue is
  full (`ws.rs:398-407`). Verified by
  `a_full_outbound_queue_closes_the_tunnel_instead_of_growing_unboundedly`.
  This bounds memory *per already-open tunnel* but nothing bounds the
  *number* of tunnels or connections in the first place (next point).
- **Connection floods / request floods — real gap, not fixed.**
  relay-protocol.md's "Errors and backpressure" section requires
  control-frame requests exceeding a per-Identity rate to close the
  connection ("rate limited per-Identity; exceeding the limit closes the
  connection"). Grepping `choosh-relayd/src` for any rate-limiting,
  connection-count cap, or per-identity request-rate tracking (`governor`,
  `Semaphore`, `RateLimit`, or hand-rolled equivalents) finds **none**.
  There is also no cap on total concurrent WebSocket connections at the
  `axum` router level (`lib.rs:73-88`, `router`), and nothing prevents the
  same authenticated Identity from opening unboundedly many `open-tunnel`
  requests (each allocates a `Tunnel` entry and, per Case 4, an 8-byte ID —
  cheap per-tunnel, but with no cap, an attacker holding one legitimate
  device credential could still grow the `tunnels`/`connections` maps
  without bound by looping `open-tunnel` against a real, online target).

**Verdict: Frame-size and per-tunnel-backpressure vectors are mitigated
(with one bug fixed this pass). Idle-partial-frame and connection/request-
flood vectors are accepted risk, named as explicit follow-ups.**

**Reasoning.** The parts of this that are load-bearing for a genuinely
malformed or oversized single frame are solid and now match spec exactly.
The parts that require *rate* or *count* limiting across many
frames/connections/tunnels are simply not implemented yet — this isn't a
subtle bug to patch, it's unbuilt functionality (a rate limiter needs a
design decision about the actual limits, not a two-line fix), so it's
reported honestly as a gap rather than downplayed. Given `relayd` is
explicitly called out in DESIGN.md §11 as now "load-bearing for the whole
fleet," this and the Case 3 revocation gap are the two findings in this
review most worth prioritizing before this system carries anything
sensitive.

---

## Summary of changes made during this review

1. **`rust/choosh-relayd/src/ws.rs`** — `serve_authenticated_loop`'s
   `FRAME_CLASS_TUNNEL` arm now explicitly enforces
   `MAX_TUNNEL_FRAME_BYTES` on ingress and closes the connection if
   violated (previously only the shared, larger `MAX_CONTROL_FRAME_BYTES`
   decoder limit was enforced on tunnel frames, letting an oversized tunnel
   frame silently tear down a tunnel instead of terminating the connection
   per relay-protocol.md). New regression test:
   `oversized_tunnel_frame_closes_the_connection` in
   `rust/choosh-relayd/src/integration_tests.rs`.
2. **`rust/choosh-relayd/src/ws.rs`** — `authenticate_device` now binds the
   authenticated `device_id` to the CA-verified `cert_device_id` directly
   rather than the caller's presented `device_auth.device_id` field
   (behaviorally identical today, since the two are checked equal just
   above, but removes the reliance on that check to stay safe under future
   edits).
3. **`docs/specs/auth-and-enrollment.md`** — corrected the laptop-proxy
   capability-table row, which had drifted stale relative to the actual M6
   implementation (missing the `list-devhost-ssh-endpoints` capability
   `proxy sync` depends on, per `ssh-bridge-and-zed.md`).

All three changes are covered by `cargo test -p choosh-relayd` (30/30
passing) and `cargo clippy -p choosh-relayd --all-targets -- -D warnings`
(clean).

## Named follow-ups (not fixed in this pass — too large for a targeted fix)

- **No working device or phone-passkey revocation mechanism.** `revoked`
  is honored everywhere it's checked but nothing ever sets it. Needs a
  passkey-gated revoke control frame/route, plus a decision on whether
  `Registry`'s current lack of disk persistence blocks trusting revocation
  as an incident-response tool.
- **No connection-flood or per-Identity request-rate limiting**, despite
  relay-protocol.md requiring it. Needs an actual rate-limiter design
  (bucket sizes, what "closes the connection" means for a legitimate but
  bursty devhost), not a quick patch.
- **No idle timeout on a stalled mid-frame connection** (as opposed to a
  stalled *tunnel*, which is covered). Low severity given the bounded
  per-partial-frame buffer size, but still an unbounded-connection-count
  vector combined with the point above.
