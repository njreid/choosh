# Resources and re-authentication

Status: Draft

## Purpose

Generalizes the existing, narrowly-scoped "SSO/cloud-CLI device-code
bridge" (M7b; `rust/choosh-hostd/src/auth_detect.rs`; `agent_events.md`'s
`auth_required` event) into a first-class **Resource** entity: a named,
typed, devhost-attached reference to something that needs occasional
human-in-the-loop re-authentication or is otherwise external
infrastructure an agent can be pointed at — an AWS SSO profile, a
`gcloud`/Firebase account, a Twilio project, or a second EC2 host stood up
for testing. Resources are declared once (by the operator, or by an agent
that hit a wall and asked), then reused: every later re-auth reuses the
same declaration instead of re-deriving "how does this CLI's flow work"
from scratch.

This spec does not change `auth_detect.rs`'s existing four-provider
detector today — it defines the shape a generalized version should take,
grounded in what real re-auth flows actually look like (see "Provider
survey" below, which corrects some assumptions the current detector's own
doc comments already flag as unverified). Implementation is intentionally
out of scope; see "Open questions" at the end.

## Why one shape doesn't fit all of these

The temptation is to model every CLI re-auth as "the CLI prints a URL and
a short code, the human visits the URL on another device, the CLI polls
and picks up automatically" — literally RFC 8628's OAuth Device
Authorization Grant. That shape is real and common, but **it is not
universal**, and treating it as universal is exactly the mistake
`auth_detect.rs`'s own doc comments already caught and reversed for
`gcloud` (see that file's `gcp_*` tests and the long comment above them).
Four structurally different patterns actually occur in CLIs you'd plausibly
run on a devhost:

| Pattern | Shape | Who types what, where | Examples |
| --- | --- | --- | --- |
| **A — self-polling device code** | CLI prints a short code + URL, blocks, polls a token endpoint in the background, exits on its own once the browser step completes | Code/URL flow devhost→phone only; nothing is ever typed back into the blocked CLI | `gh auth login --web`, `az login`, `aws sso login` (verified below — despite looking AWS-console-flavored, it's this pattern) |
| **B — manual code paste-back** | CLI prints a URL, blocks on an interactive prompt in the *same* process, human completes auth in a browser elsewhere and is shown a value that must be typed into that waiting prompt | Value flows devhost→phone→devhost, and must reach the *original* blocked process's stdin | `gcloud auth login --no-launch-browser` (classic OOB code); `gcloud auth login --no-browser`'s remote-bootstrap variant (a full command line, not a short code, but same shape) |
| **C — static secret paste, no browser flow at all** | CLI prompts for a value the human already has, or must separately fetch from a web console unrelated to anything the CLI itself printed; no polling, no code exchange | Value flows from wherever it lives (a web console, an authenticator app) into the CLI's stdin/argv; the CLI never mediates the round trip | `aws configure` (Access Key ID/Secret Access Key); `aws sts get-session-token --serial-number <mfa-arn> --token-code <code>` (the code comes from an authenticator app, not a website); Twilio CLI's Account SID/Auth Token prompt |
| **D — resume via a fresh command, not stdin** | CLI prints a URL, exits (or keeps polling as a *fallback*), and separately instructs the human to run a **new**, non-interactive invocation with a value once they have it — never types into the original blocked process | Value flows devhost→phone→devhost, but lands as an argument to a brand-new subprocess, not as injected keystrokes into a live PTY | `firebase login --no-localhost` (verified below) |

Patterns A and D are the two worth building first: neither requires
injecting synthetic keystrokes into a live, possibly-scrolled PTY session
— A needs no devhost-side action at all beyond waiting, and D just means
spawning a fresh, fully-formed command once the phone hands back a value.
B requires real PTY stdin injection into a specific still-blocked process,
which is a meaningfully bigger and riskier piece of plumbing (see "PTY
injection" under Open questions). C needs no CLI-output detection at all —
there's nothing in the terminal to pattern-match, so this only becomes
tractable once Resources are explicit, human/agent-declared entities
rather than something a text scanner infers.

## Provider survey

Verification status called out explicitly per the culture already
established in `auth_detect.rs`'s doc comments — don't trust a row here
that says "researched, not captured" the same way you'd trust one that
says "captured live."

### AWS SSO — `aws sso login [--profile NAME] [--no-browser]`

**Pattern A, verified against real, installed `aws-cli 2.35.17` source**
(this is also what `auth_detect.rs::detect_aws` already matches —
described here for completeness/contrast with AWS's other two flows
below). On a headless devhost (no `DISPLAY`, or `--no-browser` passed
explicitly), it prints a verification URL and a short code, then blocks
polling AWS's SSO OIDC token endpoint in the background — no value is
ever typed back into the CLI. Confirmed live that a bogus `sso_start_url`
reaches AWS's real OIDC endpoint and returns a genuine
`InvalidRequestException`, proving the polling is real network activity,
not a local simulation.

### AWS IAM user / "root" style static keys — `aws configure`

**Pattern C, captured live** (`aws-cli 2.35.17`, this environment):

```
AWS Access Key ID [None]:
```

followed by `AWS Secret Access Key [None]:`, `Default region name
[None]:`, `Default output format [None]:` — four sequential prompts, no
URL, no code, no polling. The values must already exist (minted once in
the AWS Console, under IAM → Users → Security credentials) — there is
nothing in the CLI's own output for a detector to key off, since the CLI
never announces where those values come from. This is the most common
shape for anything that isn't SSO/OAuth-fronted: the CLI is a passive
receiver of a secret the human already has or must go fetch.

### AWS STS session token (MFA-protected IAM user) — `aws sts get-session-token --serial-number <mfa-device-arn> --token-code <code>`

**Pattern C/D hybrid, captured live (flag shape only)**: not interactive
at all — `--token-code` must be supplied as an argument up front (there is
no blocking prompt to wait on), and its value originates on the *human's*
authenticator app (TOTP), not from anything the CLI or a website shows.
Directionally the reverse of A/B/D above: the phone is the source of
truth here, and the devhost-side command only runs once that value is in
hand — structurally identical to firebase's "resume via a fresh command"
(D), just with the value sourced from an authenticator app instead of a
web console.

### `gcloud auth login` — no verified pattern-A shape exists

**Pattern B, researched (no live `gcloud` binary available in this
environment; see `auth_detect.rs`'s own extensive comment on this exact
gap, cross-checked against Google's current reference docs)**. Two
headless sub-flows, both requiring a value typed back into the *same*
blocked process, neither ever printing a short device code:

- `--no-launch-browser`: prints a long `accounts.google.com/o/oauth2/auth?...`
  URL; the browser step ends with a code the human must copy and paste
  into the CLI's own `Enter authorization code:` prompt.
- `--no-browser` (current default fallback): prints a full `gcloud auth
  login --remote-bootstrap="..."` command to run on a *different* machine
  that has a browser; that machine's `gcloud` prints a callback URL, which
  gets pasted into the *first* machine's `Enter the output of the above
  command:` prompt. No short code anywhere — the artifact handed back is
  itself a long URL.

Either way: this is genuinely Pattern B, and needs live-binary
verification before anything built against it is trusted — flag this
explicitly if/when this gets implemented, the same way `auth_detect.rs`
already does.

### Firebase CLI — `firebase login --no-localhost`

**Pattern D, captured live** (`firebase-tools 15.27.0`, this environment):

```
To sign in to the Firebase CLI:

1. Take note of your session ID:

   180A0

2. Visit the URL below on any device and follow the instructions to get your code:

   https://auth.firebase.tools/login?code_challenge=...&session=...&attest=...

3. Complete the login by running:

   firebase login <authorizationCode>
```

Also confirmed live: the original process *does* stay running and appears
to poll in the background after printing this (backgrounded, still alive
2s later with no stdin given) — so in practice this behaves like Pattern A
most of the time, with the printed `firebase login <authorizationCode>`
instruction as an explicit, CLI-documented **Pattern D fallback** for
whenever the polling can't complete (e.g. a firewalled devhost that can
reach nothing but outbound HTTPS on a schedule, or a session that expired
waiting). Worth building against the fallback command specifically
*because* it's the one path guaranteed to work regardless of the devhost's
own network posture, rather than relying on background polling succeeding.

### GitHub CLI — `gh auth login --web`

**Pattern A, verified against real, installed `gh 2.96.0` binary, both
piped and through a real pty** — already fully captured and matched by
`auth_detect.rs::detect_github`. No changes needed; included here only for
the taxonomy's completeness.

### Azure CLI — `az login`

**Pattern A, well-corroborated but not live-verified** (`az` not
installed in this environment; see `auth_detect.rs::detect_azure`'s doc
comment — the printed message is Microsoft Entra ID's own
`/oauth2/devicecode` endpoint response text, not `az`-authored, which is a
materially stronger signal than "unchanged CLI help text").

### Twilio CLI — `twilio login`

**Pattern C, not captured (CLI not installed here), representative of the
broader "no OAuth at all, just paste a static secret" category** rather
than something specific to Twilio: prompts directly for an Account SID and
Auth Token (or API Key SID/Secret), both retrieved from the Twilio
Console, a website with no relationship to anything the CLI itself prints.
Any service with a "generate an API key in our web dashboard, paste it
into the CLI" onboarding falls into this same bucket — Stripe's CLI,
most CI providers' CLIs, etc.

## The `Resource` entity

A **Resource** is a devhost-scoped (not workspace-scoped — the
credentials these represent live at the OS/user level, in `~/.aws`,
`~/.config/gcloud`, `~/.twilio-cli`, not per-checkout) declaration of
something external that occasionally needs a human to complete a step
`choosh-hostd` can't do alone.

```jsonc
{
  "resource_id": "res-...",          // choosh-hostd-minted, opaque
  "devhost_id": "...",               // owning devhost; Resources don't move between devhosts
  "display_name": "Prod AWS SSO",    // operator- or agent-chosen, shown on the phone
  "kind": "aws-sso",                 // see "Built-in kinds" below; "custom" for anything else
  "pattern": "a",                    // "a" | "b" | "c" | "d" — which flow this needs (see taxonomy)
  "reauth_command": "aws sso login --profile prod --no-browser",
  "detect": {                        // only meaningful for pattern a/b; absent for c/d
    "marker": "Then enter the code:",
    // ... the rest of what auth_detect.rs's per-provider matchers already
    // encode today, made data instead of a hardcoded Rust match arm
  },
  "resume_command_template": null,   // pattern d only, e.g. "firebase login {code}"
  "fetch_instructions": null,        // pattern c only: human-readable "go get X from Y" text
  "mobile_profile": "work",          // "personal" | "work" | "ask" — see below
  "created_by": "agent:a3f9...",     // or "operator" — see "Agent-declared resources"
  "last_used_at": "2026-08-16T...",
  "last_verified_at": "2026-08-16T..." // null if never successfully completed
}
```

### Built-in kinds

Ship a small set of built-in `kind`s pre-populated with everything in the
"Provider survey" above (`aws-sso`, `aws-iam-key`, `aws-sts-mfa`, `gcloud`,
`firebase`, `github`, `azure`), each carrying its pattern and — where
pattern is a/b — its detection markers as *data*, not as a hardcoded
`match` arm the way `auth_detect.rs` does today. This is the actual
generalization the "agents can add resources" requirement needs: a new
provider (Twilio, Stripe, an internal SSO tool) becomes a new `kind`
definition, not a new Rust module and a recompile. `auth_detect.rs`'s
existing matchers are the right reference implementation to lift the
marker/extraction logic from — its bounds-checking and no-leakage
discipline (never "grab the rest of the line," always independently
validate each extracted field) is exactly what a data-driven detector must
preserve.

### Non-reauth Resources (test hosts, etc.)

Not every Resource is a re-auth flow. "Another EC2 host being used for
testing" is a Resource with `pattern: null` — just a named pointer to
connection info (host alias, and either a choosh devhost's own device
credential if it's itself enrolled, or a plain SSH target/key reference
otherwise). This reuses the `display_name`/`devhost_id`/`created_by`
shape above without needing any of the reauth-specific fields. Whether
this belongs in the *same* entity/table as auth resources, or a sibling
one that merely shares the "agent-addable, devhost-attached, named" shape,
is an open question below — they're similar enough to sketch together
here, but "credential re-auth" and "here's another box" are different
enough in what they're *for* that forcing one schema might be a mistake.

### Mobile profile targeting

New nuance, not present in today's `auth_required` event at all: a
Resource's re-auth may need to happen inside a specific Android **work
profile** rather than the personal one — e.g. an org's AWS SSO login is
tied to a work Google/SSO session that only exists in the work profile,
while a personal AWS root account only exists in the personal one. The
`mobile_profile` field (`personal | work | ask`) carries that as
declared metadata on the Resource.

**Real platform constraint, not yet verified against a physical
dual-profile device**: Android gives a third-party app no reliable public
API to *force* an arbitrary browser Intent into a specific other profile.
`CrossProfileApps` (API 26+) only lets an app relaunch *itself* in the
other profile, not hand off a URL to a different app running there.
What Android *does* do on its own (12+), when both profiles have a
matching browsable app installed, is offer the user a profile-switcher
chip in its own share/resolver sheet for `ACTION_VIEW` — but a caller
can't force that path to fire either. So the realistic v1 behavior is:
show the verification URL with an explicit label ("Open in your Work
profile browser") sourced from `mobile_profile`, fire a normal
`ACTION_VIEW` Intent and let Android's own resolver do whatever
cross-profile handoff it's going to do, and always provide a
copy-to-clipboard fallback so the human can paste the link into whichever
profile's browser themselves if the OS doesn't cooperate. `ask` (the
sensible default when a Resource's profile isn't known) just skips the
label and copy defaults to "current profile." This needs a real
dual-profile device test before anyone trusts the labeled-Intent path
does more than what's described here.

### Agent-declared resources

An agent that hits an auth wall it can't clear alone (a fresh `aws sso
login` prompt with no prior Resource on file, or explicit knowledge that
a task needs Twilio and none is configured) should be able to request one
be created, reusing the same mechanism already built for "ask a human a
question" (`agent-events.md`'s `input_required`,
`WireInputReason::Elicitation` fits this shape well: "I need something
from you before I can continue" is exactly what a missing/expired
Resource is). The response either supplies an existing `resource_id` to
retry against, or walks the human through declaring a new one (kind,
command, mobile profile) — at which point it's stored via the same path
an operator-declared Resource would use. `created_by` on the entity
records which case happened, mostly for audit/cleanup ("what did agents
add on their own that I should review").

## Re-auth lifecycle (per pattern)

```
idle
  │ (pattern a/b: PTY output matches a Resource's `detect` marker)
  │ (pattern c/d: explicitly triggered — operator action, or agent's
  │  input_required round trip resolves to "yes, start this Resource's
  │  reauth")
  ▼
triggered ──────────────────────────────────────────────┐
  │ hostd emits a `resource_reauth_required` agent-event  │
  │ (superset of today's `auth_required`; see Open        │
  │ questions) with resource_id, pattern, and whatever     │
  │ fields that pattern needs (url+code for a/b, fetch     │
  │ instructions for c, url+resume-template for d)         │
  ▼                                                        │
phone-notified                                             │
  │ human completes the browser/console step on their      │
  │ phone (respecting mobile_profile where the platform     │
  │ allows)                                                 │
  ▼                                                        │
  ├─ pattern a: nothing further — the original devhost      │
  │   process polls and exits on its own; hostd's job is    │
  │   just to notice it's no longer blocked (or time out)   │
  ├─ pattern b: phone hands back a value; hostd must inject  │
  │   it as stdin into the *specific* still-live PTY session │
  │   the trigger came from (needs a PTY-identity handle,    │
  │   not just "the devhost" — see Open questions)           │
  ├─ pattern c: phone (or the human directly) hands back a   │
  │   value; hostd runs the resource's reauth_command fresh, │
  │   supplying it (env var, flag, or piped stdin, per kind) │
  └─ pattern d: phone hands back a value; hostd runs          │
      resume_command_template with it substituted in, as a   │
      brand-new subprocess — no PTY injection needed at all   │
  ▼
verified (reauth_command's own exit code / a follow-up
  no-op invocation of it confirms success) or failed
  (timeout, wrong value, resumed command's own error) ───► idle
```

## Security considerations

- A Resource never stores the actual secret/credential material itself —
  only orchestration metadata (what kind, what command, what pattern,
  what marker to watch for). The credential always ends up exactly where
  the underlying CLI already puts it (`~/.aws/sso/cache`,
  `~/.config/gcloud`, etc.) — Resources describe *how to get there*, they
  are not a second, choosh-owned credential store to keep in sync or leak.
- Every requirement `agent-events.md`'s `auth_required` already states
  ("No token, credential, or session identifier MUST ever appear in this
  event") applies unchanged to `resource_reauth_required` and to whatever
  RPC carries a phone-supplied value back to hostd for patterns b/c/d —
  those payloads are exactly the kind of thing that must never be logged,
  persisted past the single re-auth attempt, or echoed back over any other
  channel.
- A compromised/malicious process inside a devhost's PTY could try to
  spoof a fake pattern-a/b prompt to phish a *real* secret out through a
  legitimate-looking phone notification (e.g. print text shaped like
  `detect_aws`'s marker, pointed at an attacker-controlled URL). Two
  independent mitigations, both already partially true of
  `auth_detect.rs`'s current bounds-checked extraction: (1) `detect`
  markers should be specific enough that generic shell output can't
  accidentally or trivially satisfy them (the existing four already do
  this reasonably well), and (2) the phone UI should always show the full
  extracted URL/domain before the human acts on it, never just a label —
  "Prod AWS SSO" is operator/agent-chosen text and MUST NOT be trusted
  as proof of where the link actually goes.

## Open questions (deliberately not decided here)

- **Wire format**: does `resource_reauth_required` fully replace
  `auth_required`, or does `auth_required` stay as a legacy/simplified
  alias for pattern-a-only cases? Leaning toward replace-and-migrate
  (`auth_required`'s four providers become four built-in `kind`s), but
  that's a wire-compat decision, not a design one, and belongs in
  `agent-events.md` once settled.
- **PTY injection for pattern b**: today's `pty:<item_id>` tunnel already
  carries human keystrokes into a live session, so the raw mechanism
  exists — the open part is identifying *which* still-running PTY a
  `detect` match came from once the phone hands a value back, especially
  if the human has since navigated away from that terminal item's screen.
  Needs its own design pass; likely the highest-effort part of this whole
  feature and the reason patterns a/d should ship first.
- **Storage**: a new `registry.rs`-style JSON store on `choosh-hostd`,
  mirroring how `Workspace`s are persisted today, is the obvious shape —
  not designed here.
- **RPC surface**: `resource.create`/`resource.list`/`resource.reauth`-
  shaped RPCs, added to `host_rpc.rs` alongside the existing
  `workspace.*`/`item.*`/`project.*` families — not designed here.
- **Android UI**: where Resources live in the app (a new top-level list?
  hung off the devhost/fleet view?), and the actual profile-picker/
  copy-fallback UI for `mobile_profile` — not designed here, and the
  platform capability itself needs verifying against a real dual-profile
  device before committing to specific UI copy.
