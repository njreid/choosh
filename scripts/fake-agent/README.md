# scripts/fake-agent/

Small, checked-in fake CLI scripts for exercising choosh-hostd's agent-event
and re-auth plumbing end to end -- including, eventually, tapping through
the real flow on a physical phone -- without needing a real agent CLI or
real AWS/gcloud/Firebase accounts. See `docs/specs/resources-and-reauth.md`
("Provider survey") for the real, live-captured or carefully-researched
shapes these scripts reproduce, and
`rust/choosh-hostd/src/auth_detect.rs` for the real pattern-A detector
these are meant to exercise faithfully, not approximate.

All four scripts are plain, ordinary foreground programs -- nothing here
does anything special to "look watched." Per this project's design
decision, patterns B/C/D are always run by `choosh-hostd` itself as a
managed child process it fully owns (piped stdin/stdout/stderr), never
detected by scanning an arbitrary human-attached terminal; pattern A is
still genuinely watched passively, the same way any real `gh`/`aws`/`az` in
an `AgentTerminal`/`Shell` item's PTY would be.

## The scripts

- **`fake-agent.sh`** -- a fake multi-step "agent task": realistic status
  lines, a configurable sleep between steps, a byte-for-byte reproduction of
  `auth_detect.rs`'s real captured `GH_PIPED_REAL` pattern-A device-code
  prompt partway through, and a plain-text-labeled "waiting for approval"
  moment later on. **That second moment is not a verified reproduction of a
  real trigger** -- see the `NOTE` comment in the script's source for why no
  such stdout-marker convention exists to reproduce (the real
  `input_required` path runs through a genuine Claude Code/Codex/OpenCode
  hook shelling out to `choosh-hostd emit`, not anything a CLI prints to its
  own stdout). Run this as the command inside an `AgentTerminal`/`Shell`
  item to smoke-test pattern-A passive detection end to end.

- **`fake-pattern-b.sh`** -- mimics `gcloud auth login --no-launch-browser`:
  prints a fake OAuth URL, then blocks on `Enter authorization code: ` (no
  trailing newline) reading one line from stdin. Exit 0 + success message on
  a non-empty line, exit 1 on EOF/empty.

- **`fake-pattern-c.sh`** -- mimics a static-secret-paste CLI (the
  `aws configure` / Twilio category), trimmed to a single prompt for this
  pass (not a full multi-field `aws configure` clone -- see the script's
  header comment). Prints `Enter your fake API key: `, reads one line. Exit
  0 + success on non-empty, exit 1 on EOF/empty.

- **`fake-pattern-d.sh`** -- mimics `firebase login --no-localhost`: no
  args prints the real captured session-ID/URL/fallback-command shape and
  then genuinely polls a local temp-file sentinel for a bounded time (see
  the script's header for why polling was chosen over print-and-exit, and
  the `FAKE_PATTERN_D_TIMEOUT_SECONDS`/`FAKE_PATTERN_D_SENTINEL` env vars);
  `fake-pattern-d.sh <code>` is the resume invocation, which writes that
  sentinel and exits 0.

Every script has a full usage/behavior comment block at the top -- read
that before running one for the first time.

## Registering one as a test Resource

Once `choosh-hostd`'s Resource RPCs land (`rust/choosh-protocol/src/host_rpc.rs`'s
`ResourcePropose`/`ResourceConfirm`/`ResourceReauthStart`/
`ResourceReauthComplete`, per `docs/specs/resources-and-reauth.md`), these
scripts are meant to be handed to `resource.propose` as the `reauth_command`
an operator (or an agent, pending human confirmation) points a Resource at.
Use an **absolute path** -- `choosh-hostd` runs `reauth_command` as a
subprocess on the devhost, not through a shell that necessarily has this
checkout's `scripts/fake-agent/` on `PATH`.

Pattern-b test Resource, as a `resource.propose` RPC request (see
`RpcRequest::ResourcePropose` in `host_rpc.rs` for the authoritative field
list):

```json
{
  "type": "resource-propose",
  "request_id": "req-1",
  "display_name": "Fake pattern-B test resource",
  "resource_kind": "custom",
  "pattern": "b",
  "reauth_command": "/home/njr/code/choosh/scripts/fake-agent/fake-pattern-b.sh",
  "mobile_profile": "ask"
}
```

Pattern-c and pattern-d follow the same shape, just pointed at
`fake-pattern-c.sh` / `fake-pattern-d.sh` with `"pattern": "c"` /
`"pattern": "d"` respectively. For pattern d specifically: per the spec's
`Resource` entity sketch, the *resume* invocation (`fake-pattern-d.sh
<code>`) is what a `resume_command_template`-shaped field would run with
the phone-supplied value substituted in (e.g.
`"/home/.../fake-pattern-d.sh {code}"`) -- as of this writing
`host_rpc.rs`'s `ResourcePropose` only has one command field
(`reauth_command`, documented there as covering patterns a/b/c), so which
field the resume command actually lands in in the real implementation is a
hostd-side detail to confirm once that RPC is built out, not something this
script directory can pin down. Either way, the command string to hand it is
the absolute path to `fake-pattern-d.sh` with `{code}`/`{value}` (whatever
the real template placeholder ends up being named) appended as an argument.

Pattern-a resources don't take a `fake-agent.sh`-shaped `reauth_command` at
all in the same sense -- pattern A is detected passively from an
`AgentTerminal`/`Shell` item's PTY output, not run as a managed subprocess.
To smoke-test pattern-A detection, just run `fake-agent.sh` as the command
of an `AgentTerminal`/`Shell` item and confirm `choosh-hostd` emits a
`resource_reauth_required` (or, pre-migration, `auth_required`) event when
it hits the `GH_PIPED_REAL`-shaped block partway through.

For a quick local smoke test without any of the above:

```sh
echo "some-fake-value" | ./scripts/fake-agent/fake-pattern-b.sh   # exit 0
./scripts/fake-agent/fake-pattern-b.sh </dev/null                 # exit 1
FAKE_AGENT_STEP_SECONDS=0 ./scripts/fake-agent/fake-agent.sh      # fast run
```
