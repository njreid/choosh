# Auth and enrollment

Status: Draft

## Purpose

Makes precise the trust model summarized in [DESIGN.md](../../DESIGN.md) §5
and §11: passkeys for every human-facing surface, device credentials minted
from a passkey-authenticated session for every machine-facing surface, and
no password anywhere in the system, ever. This spec covers `relayd`'s
WebAuthn relying-party behavior, enrollment-token issuance, devhost and
laptop-proxy credential exchange, and revocation. It does not cover the
frame-level protocol those exchanges ride on (see
[relay-protocol.md](relay-protocol.md)).

## Identity classes and capability scopes

`relayd` recognizes three Identity classes, each with a fixed capability
scope — a connection's class is set once at enrollment and never escalates:

| Class | Authenticates via | May do |
| --- | --- | --- |
| `phone` (human) | WebAuthn passkey | `list-devhosts`, `open-tunnel` to any devhost, `request-enrollment-token`, `register-fcm-token`, receive `agent-event` forwards |
| `laptop-proxy` (machine) | Device credential from enrollment | `open-tunnel` to devhost SSH endpoints only (`purpose = "ssh"`); `list-devhost-ssh-endpoints` (a restricted read of alias + SSH host key per devhost, for `proxy sync`, per [ssh-bridge-and-zed.md](ssh-bridge-and-zed.md)); nothing else |
| `devhost` (machine) | Device credential from enrollment | `agent-event`, accept inbound tunnels, `open-tunnel` to another devhost only for cross-host offload (`purpose = "offload"`) |

A `laptop-proxy` connection MUST NOT be able to call `list-devhosts` or open
a non-`ssh`-purpose tunnel; a `devhost` connection MUST NOT be able to open
a tunnel to a `phone` or `laptop-proxy` Identity. `relayd` enforces this in
`open-tunnel` handling, not by client-side convention.

## WebAuthn (phone / web)

`relayd` is a WebAuthn relying party (RP) via `webauthn-rs`, RP ID bound to
`relayd`'s own domain. There is exactly one registered user (single-tenant,
per DESIGN.md §5) who may hold multiple passkey credentials (one per
enrolled phone/browser).

- **Registration** (first phone, or adding a second device): standard
  WebAuthn attestation ceremony via Android Credential Manager
  (`CreatePublicKeyCredentialRequest`) or a browser's platform
  authenticator, with resident keys (discoverable credentials) required, no
  additional attestation-format restriction. On success, `relayd` mints a
  long-lived session credential (an opaque bearer token, not the passkey
  itself) and returns it; the app stores it in Android Keystore.
- **Reuse**: every later app open presents the stored session credential
  directly over the WebSocket handshake — no WebAuthn ceremony repeats
  unless the credential has been revoked or has expired (session
  credentials are valid 90 days, silently renewed on use so an
  actively-used phone never re-prompts, but a phone unused for 90+ days
  must re-assert).
- **Re-auth / explicit revoke**: a stored session credential that fails
  (revoked, expired) forces a WebAuthn assertion ceremony
  (`GetPublicKeyCredentialOption`) before any further request succeeds.
- Web access (e.g. the Zellij-web-client break-glass path, per
  [ssh-bridge-and-zed.md](ssh-bridge-and-zed.md)) uses the same RP and the
  browser's platform authenticator; there is no separate "web account."

## Enrollment tokens

A `phone` connection may call `request-enrollment-token` to obtain a token
that lets one fresh machine Identity enroll. Properties, all enforced
server-side:

- **Single-use**: consumed atomically on the first `enroll` exchange that
  presents it; a second use fails closed with no partial effect.
- **Short-lived**: 15 minutes from issuance. Long enough to paste into a
  running install command by hand; short enough that a token leaked via
  shell history, a screen share, or a compromised phone during that window
  is the entire blast radius, not a standing credential.
- **Issuable only from an authenticated `phone` connection** — never from a
  `laptop-proxy` or `devhost` connection, and never via any unauthenticated
  path.
- **Scoped to one identity class** at issuance time (`devhost` or
  `laptop-proxy`, per the `request-enrollment-token` request field); the
  resulting `enroll` exchange MUST fail if the presented token's class
  doesn't match the identity class the caller is enrolling as.

## Devhost enrollment

1. The operator runs `request-enrollment-token` from the phone (or the
   install script does it as a documented manual pre-step — the token
   itself, not a passkey, is what gets typed/pasted into
   `curl ... | sudo sh -s -- --token=<token>`).
2. `choosh-hostd` generates a fresh Ed25519 keypair locally; the private
   key never leaves the device.
3. `choosh-hostd` sends `enroll { token, identity_class: "devhost",
   public_key }`.
4. `relayd` validates the token, then signs a short certificate binding
   `public_key` to a freshly assigned `device_id` using `relayd`'s own
   internal enrollment CA (an Ed25519 signing key `relayd` holds; this is
   not a public CA and issues no certificates outside this system).
   Certificate validity: 1 year, auto-renewable by the device before
   expiry over an already-authenticated connection (a renewal is not a
   re-enrollment — it doesn't need a new token).
5. `relayd` returns `{ device_id, certificate }`. `choosh-hostd` persists
   both locally (alongside the private key) and uses the certificate to
   authenticate every future connection: on connect, it presents
   `{ device_id, certificate, signature }` where `signature` is a fresh
   signature over a `relayd`-issued challenge nonce, proving possession of
   the private key on every connection, not just at enrollment.
6. As part of this same exchange, `choosh-hostd` includes its loopback SSH
   server's host public key (see
   [ssh-bridge-and-zed.md](ssh-bridge-and-zed.md)). `relayd` stores it
   against this `device_id`, signed by the device's own new certificate —
   this is the one moment a devhost's SSH host key is ever established as
   trusted, and it happens over the same authenticated channel as the rest
   of enrollment, not a separate TOFU prompt anywhere. **Design decision
   (M6):** the SSH host key is not a second, independently generated
   keypair — `choosh-hostd` derives its SSH host key directly from this
   same Ed25519 enrollment credential (`rust/choosh-hostd/src/ssh_keys.rs`),
   since a distinct keypair would need the same generation, persistence,
   and revocation lifecycle as the enrollment key anyway, with no
   independent rotation benefit.
7. `alias`, `platform`, and `account_label` (for presence/fleet display)
   are set at this point too, either from install-script flags or
   defaulted from OS/cloud metadata `choosh-hostd` can read locally
   (instance tags, hostname) — never trusted blindly for anything
   security-relevant, only for display.

## Laptop-proxy enrollment

Identical exchange (`enroll { token, identity_class: "laptop-proxy",
public_key }` → `{ device_id, certificate }`), run once via
`choosh-hostd proxy enroll --token=<token>`. The resulting credential
authenticates every future `choosh-hostd proxy connect`/`proxy sync`
invocation the same way — challenge/signature on connect, same 1-year
renewable certificate.

`proxy sync` is what turns this credential into a usable `~/.ssh/known_hosts`
and `~/.ssh/config`: it opens an authenticated connection, calls
`list-devhosts`-equivalent for the fleet (a `laptop-proxy` connection is
permitted a restricted read of alias + SSH host key per devhost, even
though it cannot call the phone's `list-devhosts` presence RPC), and writes
one `known_hosts` line and one `Host` block per devhost. The trust chain
end to end: a human completed a WebAuthn ceremony once → that session
issued this laptop's enrollment token → this laptop's `enroll` exchange
proved possession of its own private key → the SSH host key it now writes
to `known_hosts` was itself established the same way, at that devhost's own
enrollment. No step in this chain is a manually confirmed fingerprint.

## Revocation

`relayd` is the sole source of truth for whether a `device_id` (or the
phone's registered passkey credential) is still valid — it does not rely on
certificate expiry as the only revocation mechanism, since a 1-year
certificate lifetime is too long a window to wait out a compromise. An
operator-initiated revoke (from the phone/web, itself passkey-gated) removes
the `device_id` from `relayd`'s active-identity registry immediately.

- A revoked device's *next* connection attempt fails closed at the
  challenge/signature step (`relayd` checks registry membership before
  accepting the signature, not just certificate validity) — it does not
  get a partial or degraded session, and no in-flight tunnels for that
  device survive revocation (they're closed per
  relay-protocol.md's reconnect-discontinuity rule).
- A revoked phone passkey credential invalidates that credential's stored
  session token immediately; the app is forced back through a fresh
  WebAuthn ceremony, which fails if the passkey itself was also removed
  from the RP's registered-credential list.
- Revocation is not currently retried/propagated to devices that are
  offline at revoke time by any push mechanism other than "their next
  connection attempt is rejected" — there is no requirement to actively
  notify a revoked device, since it has no standing capability to exercise
  in the meantime (it isn't connected).

**Not yet implemented**: the operator-initiated revoke operation itself —
a control frame or HTTP route that actually sets a device or phone
credential revoked — does not exist in `choosh-relayd` today.
`EnrolledDevice.revoked` and phone-session validity are checked everywhere
the bullets above describe, so a device that *is* revoked correctly fails
closed, but nothing in the codebase ever performs the revoke. See
`docs/security/relayd-threat-model.md` (Case 3) and
[PLAN.md](../../PLAN.md)'s Known follow-ups.

## Explicit non-goal

There is no password-based authentication path anywhere in this system —
not for initial setup, not as a fallback when a passkey is unavailable, and
not for any administrative or emergency access. The only way to gain a new
credential is to already hold one (WebAuthn on the phone, or an
enrollment token minted by an already-authenticated phone/web session).
