//! Detects a headless device-code SSO/cloud-CLI auth flow in a stream of
//! terminal bytes, per `docs/specs/agent-events.md`'s `auth_required` and
//! `docs/milestones/M7-fleet-and-provisioning.md`'s "SSO/cloud-CLI
//! device-code bridge".
//!
//! **What this observes, and what it doesn't.** The four named tools (`aws
//! sso login`, `gcloud auth login`, `az login`, `gh auth login`) share a
//! real signal when they can't hand off to a local browser: they print a
//! short-lived user code and a verification URL, then block. This module
//! never runs or knows about any of those binaries — it is a pure text
//! scanner over whatever bytes a real pty happened to produce, wired in by
//! `serve.rs`'s `open_pty_tunnel` (the same pty master `pty.rs`'s
//! `PtySession` reads for the `pty:<item_id>` tunnel path an `AgentTerminal`/
//! `Shell` item's Zellij tab is attached through). See `serve.rs`'s own doc
//! comments for exactly which spawn paths feed this and which don't.
//!
//! **Verification status per provider — read before trusting a pattern
//! here against a real device.** `aws` and `github` are matched against
//! byte-for-byte real captured output (see each `detect_*` function's doc
//! comment for how it was captured); `azure` is matched against the
//! device-code message's well-documented, years-stable phrasing
//! (cross-checked via public documentation, not captured from a live `az`
//! binary — none was installed in the environment this was built in, and
//! none was installed to check this: see [`detect_azure`]'s doc comment for
//! why, and for the additional research corroboration found while
//! rechecking this). **`gcp` has no matcher at all** — there is no
//! `detect_gcp` function and no `Gcp` arm in [`AuthCodeDetector::feed`]'s
//! dispatch chain, deliberately. See the doc comment on the `gcp_*` tests
//! near the bottom of this file for why: the real, currently-shipping
//! `gcloud` device-login flows do not print anything this module's
//! `(provider, user_code, verification_uri)` shape could ever hold, so a
//! matcher here would necessarily be unverifiable scaffolding pretending to
//! be a working detector. `"gcloud"` remains a valid built-in `resource_kind`
//! (`resource_kinds.rs`, pattern b) — it is simply never a `DetectedProvider`
//! this module's own detectors produce.
//!
//! **Extraction, not capture.** Every `detect_*` function returns only the
//! three named fields, each independently bounds- and shape-checked before
//! being accepted — never "the rest of the line" or "the whole matched
//! region". This is deliberate: agent-events.md's hard requirement ("No
//! token, credential, or session identifier MUST ever appear in this
//! event") means a naive "grab everything after the marker up to the next
//! newline" implementation would be a real defect the moment some
//! provider's real output puts anything else on that line. See this
//! module's `no_leakage` test for a concrete adversarial case.

/// Which of this module's three real detectors matched — mirrors the
/// `resource_kind` strings `resource_kinds.rs`'s built-in kind table uses
/// (`"github"`, `"aws-sso"`, `"azure"`), kept as a small closed enum here
/// (rather than a bare `&str`) so [`AuthCodeDetector::feed`]'s own
/// `already_fired` re-emission suppression can cheaply `Eq`/`Hash` on it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum DetectedProvider {
    Github,
    AwsSso,
    Azure,
}

impl DetectedProvider {
    /// The `resource_kind` string `resources.rs`'s built-in-kind lookup and
    /// `WireAgentEvent::ResourceReauthRequired::resource_kind` use for this
    /// provider.
    #[must_use]
    pub fn resource_kind(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::AwsSso => "aws-sso",
            Self::Azure => "azure",
        }
    }
}

/// One complete, validated pattern-a device-code prompt this module found
/// in a pty's output stream. Deliberately **not** a `WireAgentEvent`: this
/// module has no notion of Resources, registered or otherwise (see this
/// module's own top-level doc comment — it is a pure text scanner and
/// always has been). Turning this into the wire event
/// `docs/specs/resources-and-reauth.md` defines — looking up a matching
/// registered Resource for `resource_id`/`display_name`/`mobile_profile`,
/// or synthesizing a transient one if none is registered yet — is
/// `resources.rs`'s job (see its `event_for_pattern_a_detection`), not this
/// one's. Kept as its own named type, not a bare tuple, so that boundary is
/// explicit at every call site.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DetectedPrompt {
    pub provider: DetectedProvider,
    pub user_code: String,
    pub verification_uri: String,
}

/// Upper bound on how much recently-seen terminal text this detector keeps
/// around across [`AuthCodeDetector::feed`] calls. Bounds memory for a
/// long-lived interactive session (a `Shell` item can stay attached for
/// hours) while comfortably exceeding the longest real prompt text any of
/// the four providers print (the AWS `PrintOnlyHandler` message, the
/// longest of the four verified/researched here, is well under 1KiB).
const MAX_BUFFER_CHARS: usize = 8192;

/// Incremental scanner over one pty's output stream. Bytes arrive in
/// arbitrary chunks (`pty.rs`'s reader loop uses an 8KiB buffer, but a
/// provider's own stdout buffering can split a marker or a code across
/// reads in either direction) — this type exists specifically so a match
/// spanning a chunk boundary is still found, without ever re-scanning from
/// scratch on every byte.
pub struct AuthCodeDetector {
    /// Accumulated, ANSI-stripped, CR-normalized text seen so far, capped
    /// at [`MAX_BUFFER_CHARS`] (oldest content evicted first).
    buffer: String,
    /// The last `(provider, user_code)` this detector already emitted an
    /// event for. A completed device-code prompt typically stays visible
    /// in the pty's scrollback (and therefore in `buffer`) for as long as
    /// the CLI keeps blocking on it, which would otherwise re-match and
    /// re-emit on every subsequent read — this suppresses that re-emission
    /// for the *same* code. A genuinely new login attempt (a different
    /// code) is still reported.
    already_fired: Option<(DetectedProvider, String)>,
}

impl Default for AuthCodeDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthCodeDetector {
    #[must_use]
    pub fn new() -> Self {
        Self { buffer: String::new(), already_fired: None }
    }

    /// Feeds newly-read pty bytes. Returns `Some` the first time a
    /// complete, validated device-code prompt is recognized for a given
    /// `(provider, user_code)` pair; `None` otherwise — including while a
    /// real prompt is only partially buffered (see this module's tests for
    /// the partial-output cases this is required not to fire on).
    pub fn feed(&mut self, bytes: &[u8]) -> Option<DetectedPrompt> {
        // Lossy is correct here: a chunk boundary can legitimately split a
        // multi-byte UTF-8 codepoint, which `from_utf8_lossy` turns into a
        // replacement character for *this* feed call rather than an error —
        // acceptable because none of the four providers' device-code
        // markers or codes are anything but ASCII, so a stray replacement
        // character elsewhere in the stream can never hide a real match.
        let decoded = String::from_utf8_lossy(bytes);
        let cleaned = strip_ansi_and_normalize(&decoded);
        self.buffer.push_str(&cleaned);
        if self.buffer.chars().count() > MAX_BUFFER_CHARS {
            let overflow = self.buffer.chars().count() - MAX_BUFFER_CHARS;
            self.buffer = self.buffer.chars().skip(overflow).collect();
        }

        // No `detect_gcp` here, deliberately — see this module's top-level
        // doc comment and the `gcp_*` tests below for why `gcp` has no
        // matcher to dispatch to.
        let (provider, user_code, verification_uri) =
            detect_github(&self.buffer).or_else(|| detect_aws(&self.buffer)).or_else(|| detect_azure(&self.buffer))?;

        if self.already_fired.as_ref() == Some(&(provider, user_code.clone())) {
            return None;
        }
        self.already_fired = Some((provider, user_code.clone()));
        Some(DetectedPrompt { provider, user_code, verification_uri })
    }
}

/// Strips ANSI escape sequences (CSI — `ESC [ ... <final byte 0x40-0x7E>`,
/// e.g. SGR color codes and the cursor/terminal-capability queries real
/// CLIs emit like `ESC[6n`; OSC — `ESC ] ... (BEL | ESC \\)`, e.g. the
/// background-color query `gh` was directly observed emitting) and
/// normalizes line endings (bare `\r` dropped, `\r\n` collapsed to `\n`) so
/// the `detect_*` matchers below can work against plain text the way a
/// real terminal emulator would render it, rather than against raw
/// escape-sequence-laden bytes. Deliberately conservative on anything that
/// isn't a recognized CSI/OSC shape (a bare `ESC` followed by one other
/// character is dropped as a simple two-byte escape) rather than
/// attempting to be a complete terminal emulator.
///
/// `pub(crate)`, not private: `resource_reauth.rs`'s managed-subprocess
/// output scanning (patterns b/d, per `docs/specs/resources-and-reauth.md`)
/// reuses this exact helper rather than re-implementing ANSI stripping a
/// second time — see that module's own doc comment.
pub(crate) fn strip_ansi_and_normalize(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    loop {
                        match chars.next() {
                            None | Some('\u{7}') => break,
                            Some('\u{1b}') if chars.peek() == Some(&'\\') => {
                                chars.next();
                                break;
                            }
                            Some(_) => {}
                        }
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
            continue;
        }
        if ch == '\r' {
            continue;
        }
        out.push(ch);
    }
    out
}

/// A conservative shape check for the short alphanumeric(-dash) code every
/// one of these providers prints: long enough not to match stray words,
/// short enough to bound what a malformed/adversarial stream could get
/// accepted as a "code" (see this module's `no_leakage` test), and
/// alphanumeric-plus-dash only — real device codes never contain spaces or
/// punctuation, so if the token we found doesn't look like this, treat the
/// "match" as spurious rather than forwarding something wrong.
fn looks_like_device_code(token: &str) -> bool {
    let len = token.len();
    (4..=24).contains(&len) && token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') && token.bytes().any(|b| b.is_ascii_alphanumeric())
}

/// Returns the whitespace-delimited token starting at `text[start..]`, with
/// common trailing sentence punctuation trimmed — but only if that token is
/// already terminated by whitespace somewhere in `text`. Returning `None`
/// when the token runs all the way to the end of the buffer (no confirmed
/// terminator yet) is the load-bearing part: it's what makes a code or URL
/// that's been cut off mid-stream by a chunk boundary correctly *not*
/// match yet, rather than being extracted truncated.
fn take_token(text: &str, start: usize) -> Option<&str> {
    let rest = text.get(start..)?;
    let rest = rest.trim_start_matches([' ', '\t']);
    let end = rest.find(char::is_whitespace)?;
    let token = rest[..end].trim_end_matches(['.', ',', ':', ';', '!', '?']);
    if token.is_empty() { None } else { Some(token) }
}

/// The first whitespace-delimited word on the first non-blank line at or
/// after byte offset `from` — used for the AWS matcher, whose code sits
/// alone on its own line after a blank line. Like [`take_token`], requires
/// a confirmed trailing newline for both any blank lines skipped and the
/// token's own line, so a still-arriving line never matches early.
fn first_nonblank_line_token(text: &str, from: usize) -> Option<&str> {
    let mut pos = from;
    loop {
        let remaining = text.get(pos..)?;
        let newline_offset = remaining.find('\n')?;
        let line = remaining[..newline_offset].trim();
        if line.is_empty() {
            pos += newline_offset + 1;
            continue;
        }
        let end = line.find(char::is_whitespace).unwrap_or(line.len());
        let token = line[..end].trim_end_matches(['.', ',', ':', ';', '!', '?']);
        return if token.is_empty() { None } else { Some(token) };
    }
}

/// The first `https://` URL in `text`, terminated by whitespace (i.e. not
/// yet-incomplete), bounded to a sane length so a pathological stream
/// can't make this scan (or a downstream consumer) do unbounded work.
///
/// `pub(crate)`, not private: `resource_reauth.rs`'s pattern-b/d
/// verification-URL extraction reuses this exact helper — same
/// bounds-checking discipline, one implementation. See
/// [`strip_ansi_and_normalize`]'s doc comment for the same reasoning.
pub(crate) fn first_https_url(text: &str) -> Option<&str> {
    let start = text.find("https://")?;
    https_url_at(text, start)
}

/// The *last* `https://` URL in `text` — used where a provider prints the
/// verification URL before the code (AWS): with [`detect_aws`] anchoring on
/// the *last* (most recent) occurrence of their code marker via `str::rfind`
/// (so a buffer holding more than one completed login attempt still
/// reports the current one, not a stale earlier one), the URL that
/// belongs to that same, most-recent attempt is symmetrically the nearest
/// one preceding it, i.e. the last one in the prefix searched.
fn last_https_url(text: &str) -> Option<&str> {
    let start = text.rfind("https://")?;
    https_url_at(text, start)
}

fn https_url_at(text: &str, start: usize) -> Option<&str> {
    let rest = &text[start..];
    let end = rest.find(char::is_whitespace)?;
    let url = &rest[..end];
    if url.len() > 512 { None } else { Some(url) }
}

/// **Verified against real, captured output of the installed `gh` binary
/// in this environment** (`gh version 2.96.0`), both without a controlling
/// terminal (`gh auth login --hostname github.com --git-protocol https
/// --web` piped, browser suppressed via `BROWSER=/bin/false`) and — the
/// case that actually matters here, since a `pty:<item_id>` tunnel always
/// gives the attached client a real controlling terminal — through a real
/// pty (`python3`'s `pty.fork()`), byte-for-byte, including the ANSI SGR
/// color codes and the `ESC]11;?ESC\`/`ESC[6n` terminal-capability queries
/// `gh` emits first. The two captured forms:
///
/// ```text
/// ! First copy your one-time code: F1FE-DF1C
/// Open this URL to continue in your web browser: https://github.com/login/device
/// ```
///
/// and, over a real pty (color codes shown stripped, as
/// [`strip_ansi_and_normalize`] leaves them):
///
/// ```text
/// ! First copy your one-time code: 21F9-D986
/// Press Enter to open https://github.com/login/device in your browser...
/// ```
///
/// Both share the same stable anchor phrase and both put the verification
/// URL somewhere after the code — this single matcher covers both real
/// captured shapes (and a GitHub Enterprise `--hostname`, which changes the
/// URL's host but not this structure) without needing to special-case tty
/// vs. non-tty.
fn detect_github(text: &str) -> Option<(DetectedProvider, String, String)> {
    const MARKER: &str = "First copy your one-time code:";
    // The *last* occurrence: a long-lived buffer can accumulate more than
    // one completed prompt (a retried login), and the current one is
    // always the most recent — see `last_https_url`'s doc comment for the
    // matching reasoning on the AWS/gcp side.
    let marker_pos = text.rfind(MARKER)?;
    let code = take_token(text, marker_pos + MARKER.len())?;
    if !looks_like_device_code(code) {
        return None;
    }
    let code_end = marker_pos + MARKER.len() + text[marker_pos + MARKER.len()..].find(code)? + code.len();
    let uri = first_https_url(&text[code_end..])?;
    if !uri.contains("/login/device") {
        return None;
    }
    Some((DetectedProvider::Github, code.to_string(), uri.to_string()))
}

/// **Verified against the real, installed `aws` CLI's source** (`aws-cli
/// 2.35.17`, `awscli/customizations/sso/{login,utils}.py` read directly
/// from this environment's site-packages — not documentation, the literal
/// f-strings the running binary formats and prints) for both handlers
/// `aws sso login` can use:
///
/// `PrintOnlyHandler` (selected by `--no-browser`, the flag a genuinely
/// headless devhost's launcher would reach for):
///
/// ```text
/// Browser will not be automatically opened.
/// Please visit the following URL:
///
/// {verificationUri}
///
/// Then enter the code:
///
/// {userCode}
///
/// Alternatively, you may visit the following URL which will autofill the
/// code upon loading:
/// {verificationUriComplete}
/// ```
///
/// `OpenBrowserHandler` (the default — reached on a devhost with no
/// `DISPLAY`/`WAYLAND_DISPLAY` where `webbrowser.open_new_tab` silently
/// fails, since that failure is only logged at `DEBUG`, per
/// `login.py`/`utils.py`'s `except Exception: LOG.debug(...)`):
///
/// ```text
/// Attempting to open your default browser.
/// If the browser does not open or you wish to use a different device to
/// authorize this request, open the following URL:
///
/// {verificationUri}
///
/// Then enter the code:
///
/// {userCode}
/// ```
///
/// A real device authorization against a live AWS SSO instance was not
/// available to capture end to end (confirmed instead, live, that a bogus
/// `sso_start_url` reaches the real AWS SSO OIDC endpoint and returns a
/// real `InvalidRequestException` — see this module's
/// `real_aws_error_output_is_not_a_false_positive` test for that exact
/// captured negative fixture), so the `{verificationUri}`/`{userCode}`
/// values in this module's positive AWS tests are realistic placeholders
/// substituted into the verified real template, not a captured live
/// transcript.
///
/// Both handlers share "Then enter the code:" as a stable anchor, with the
/// code alone on the next non-blank line, and the verification URL — not
/// `verificationUriComplete`, which appears later and is not what this
/// extracts — as the first `https://` URL appearing anywhere before that
/// anchor.
fn detect_aws(text: &str) -> Option<(DetectedProvider, String, String)> {
    const MARKER: &str = "Then enter the code:";
    let marker_pos = text.rfind(MARKER)?;
    let code = first_nonblank_line_token(text, marker_pos + MARKER.len())?;
    if !looks_like_device_code(code) {
        return None;
    }
    let uri = last_https_url(&text[..marker_pos])?;
    Some((DetectedProvider::AwsSso, code.to_string(), uri.to_string()))
}

/// **Still not verified against a real `az` binary, as of a second pass
/// specifically checking whether one could be captured here.** The Azure
/// CLI is not installed in this environment (`az --version` -> command not
/// found), the environment has no passwordless `sudo` (`sudo -n true` ->
/// "a password is required") so `dnf`/`yum` are not usable to install it,
/// and this environment's root filesystem had only ~2.3GiB free at the time
/// this was checked (`df -h /` — a shared tree with several other
/// concurrent tasks writing to it) — too little headroom to responsibly
/// attempt azure-cli's `pip install` route either, which pulls in a large,
/// multi-package dependency tree. None of that is a fundamental blocker (a
/// machine with `sudo` and disk headroom could add Microsoft's package repo
/// and install `azure-cli` cleanly), just not something to force here.
///
/// Matched instead against `az login`'s device-code message, cross-checked
/// via public documentation/search rather than a live capture:
///
/// ```text
/// To sign in, use a web browser to open the page https://microsoft.com/devicelogin and enter the code <CODE> to authenticate.
/// ```
///
/// This message is unusually well-corroborated for something unverified
/// against a real binary: it is not `az`-specific client-side text at all
/// — it is the `message` field Microsoft Entra ID's own `/oauth2/devicecode`
/// endpoint returns as part of the OAuth 2.0 device authorization response
/// body (RFC 8628), which `az` (via MSAL) prints verbatim rather than
/// composing itself. That means the exact phrasing is shared, and has
/// stayed stable, across every Microsoft tool that speaks this same
/// server-driven flow — `az`, the `Az` PowerShell module, Visual Studio,
/// etc. — not just `az`'s own release history, which is a materially
/// stronger signal than "one CLI's help text hasn't changed in a while."
///
/// Unlike the other three providers, both fields sit on one line — the URL
/// is the token right after "use a web browser to open the page ", the
/// code is the token right after the subsequent "enter the code ".
fn detect_azure(text: &str) -> Option<(DetectedProvider, String, String)> {
    const URL_MARKER: &str = "use a web browser to open the page ";
    const CODE_MARKER: &str = "enter the code ";
    let url_marker_pos = text.rfind(URL_MARKER)?;
    let after_url_marker = url_marker_pos + URL_MARKER.len();
    let uri = take_token(text, after_url_marker)?;
    if !uri.starts_with("https://") {
        return None;
    }
    let code_marker_pos = after_url_marker + text[after_url_marker..].find(CODE_MARKER)?;
    let code = take_token(text, code_marker_pos + CODE_MARKER.len())?;
    if !looks_like_device_code(code) {
        return None;
    }
    Some((DetectedProvider::Azure, code.to_string(), uri.to_string()))
}

// **There is deliberately no `detect_gcp` function.** An earlier version of
// this module shipped one, matching a hypothetical RFC 8628 ("OAuth 2.0
// Device Authorization Grant")-shaped message — the shape
// `agent-events.md`'s own example payload (`user_code: "WDJB-MJHT"`,
// literally RFC 8628 §3.3's example value) appears to assume `gcp` would
// look like. It was flagged even then as unverified against any real
// `gcloud` binary. A closer look at `gcloud`'s actual, currently-shipping
// behavior shows that matcher could never fire on real output, for a
// structural reason no amount of pattern tuning fixes: `gcloud auth login`
// does not use the "CLI prints a short code, user types it into a browser"
// flow the other three providers use and this module's
// `(provider, user_code, verification_uri)` return shape assumes. Both of
// its headless flows run in the *opposite* direction — research (Google's
// own `gcloud auth login`/`auth application-default login` reference docs,
// cross-checked across multiple pages, no live `gcloud` binary available in
// this environment either) turned up two of them, and neither ever prints a
// short device code to `gcloud`'s own stdout for this module to extract:
//
// - `--no-launch-browser` (the flag this module's earlier scaffolding was
//   written against): `gcloud` prints a long
//   `https://accounts.google.com/o/oauth2/auth?...` authorization URL, the
//   user completes sign-in in a browser on a *different* device, that
//   browser page then displays a verification code, and the user pastes
//   *that* code back into `gcloud`'s own "Enter authorization code:"
//   terminal prompt — the code flows from the user into `gcloud`, never out
//   of it.
// - `--no-browser` (current `gcloud`'s replacement flow): `gcloud` prints a
//   long `gcloud auth login --remote-bootstrap=...` command for the user to
//   copy and run on a *different*, browser-having machine; that machine's
//   `gcloud` prints a long callback URL, which the user pastes back into
//   the first machine's "Enter the output of the above command" prompt. No
//   short code is printed by either side at all — the artifact handed back
//   is itself a full URL.
//
// Either way, there is no `user_code` value anywhere in the stream for a
// detector to find — the field `agent-events.md`'s `auth_required` event
// requires is simply never produced by real `gcloud` output. Representing
// this flow correctly would need a different event shape entirely (e.g. an
// optional/absent `user_code`, or a distinct "paste this back" event kind),
// which is a wire-format change to `choosh_protocol::relay::WireAgentEvent`
// — out of scope here. So rather than keep a matcher that can only ever be
// exercised by a constructed fixture and never by anything `gcloud` really
// prints (see this module's `gcp_realistic_no_launch_browser_output_is_not_a_false_positive`
// and `gcp_realistic_no_browser_remote_bootstrap_output_is_not_a_false_positive`
// tests below, which assert exactly that non-match against realistic,
// research-based reconstructions of both flows), this module ships no `gcp`
// matcher and [`AuthCodeDetector::feed`]'s dispatch chain has no `gcp` arm.
// `"gcloud"` remains a valid built-in `resource_kind` (`resource_kinds.rs`,
// pattern b, per `docs/specs/resources-and-reauth.md`'s survey) — it is
// simply never something this module's own text scanner detects.

#[cfg(test)]
mod tests {
    use super::*;

    /// Byte-for-byte what `gh auth login --hostname github.com
    /// --git-protocol https --web` printed with stdout piped (no
    /// controlling terminal), `BROWSER=/bin/false` so it can't actually
    /// launch anything — captured directly against the real `gh 2.96.0`
    /// binary installed in this environment.
    const GH_PIPED_REAL: &str = "\n! First copy your one-time code: F1FE-DF1C\nOpen this URL to continue in your web browser: https://github.com/login/device\n";

    /// The same real `gh` invocation, this time through a real pty
    /// (`python3 pty.fork()`), with ANSI SGR color codes and the leading
    /// `ESC]11;?ESC\`/`ESC[6n` capability-query escapes `gh` emits first —
    /// exactly the shape `pty.rs`'s real pty master would deliver to this
    /// detector. Given here already run through [`strip_ansi_and_normalize`]
    /// is *not* what's tested below (the raw bytes are, via
    /// `feed_in_chunks_of`) — this constant documents the cleaned form for
    /// readability.
    const GH_PTY_REAL_RAW: &[u8] = b"\x1b]11;?\x1b\\\x1b[6n\r\n\x1b[0;33m!\x1b[0m First copy your one-time code: \x1b[0;1;39m21F9-D986\x1b[0m\r\n\x1b[0;1;39mPress Enter\x1b[0m to open https://github.com/login/device in your browser... ";

    /// The real `awscli 2.35.17` `PrintOnlyHandler` template
    /// (`awscli/customizations/sso/utils.py`), with a placeholder
    /// `verificationUri`/`userCode` substituted in — see [`detect_aws`]'s
    /// doc comment for why the values themselves aren't a live capture.
    fn aws_print_only_handler_output(verification_uri: &str, user_code: &str, verification_uri_complete: &str) -> String {
        format!(
            "Browser will not be automatically opened.\nPlease visit the following URL:\n\n{verification_uri}\n\nThen enter the code:\n\n{user_code}\n\nAlternatively, you may visit the following URL which will autofill the code upon loading:\n{verification_uri_complete}\n"
        )
    }

    /// The real `awscli 2.35.17` `OpenBrowserHandler` template, same
    /// source file.
    fn aws_open_browser_handler_output(verification_uri: &str, user_code: &str) -> String {
        format!(
            "Attempting to open your default browser.\nIf the browser does not open or you wish to use a different device to authorize this request, open the following URL:\n\n{verification_uri}\n\nThen enter the code:\n\n{user_code}\n"
        )
    }

    /// Byte-for-byte what `aws sso login --profile testsso --no-browser`
    /// printed against a deliberately-bogus `sso_start_url`, captured live
    /// against the real, installed `aws-cli 2.35.17` — proof this really
    /// reaches AWS's SSO OIDC endpoint and gets a real error back, and a
    /// genuine negative fixture: a *different* kind of SSO-adjacent
    /// failure that must not be mistaken for a device-code prompt.
    const AWS_REAL_INVALID_START_URL_ERROR: &str = "aws: [ERROR]: An error occurred (InvalidRequestException) when calling the StartDeviceAuthorization operation:\n\nAdditional error details:\nerror: invalid_request\nerror_description: Invalid start url provided\n";

    /// The well-documented, stable `az login` device-code message — see
    /// [`detect_azure`]'s doc comment for its verification status.
    fn az_device_code_output(code: &str) -> String {
        format!("To sign in, use a web browser to open the page https://microsoft.com/devicelogin and enter the code {code} to authenticate.\n")
    }

    /// Feeds `bytes` through a fresh detector split into `chunk_size`-byte
    /// pieces, returning the first `Some` result (if any) — used to prove
    /// detection survives an arbitrary chunk boundary, including one that
    /// lands mid-marker or mid-code.
    fn feed_in_chunks_of(bytes: &[u8], chunk_size: usize) -> Option<DetectedPrompt> {
        let mut detector = AuthCodeDetector::new();
        for chunk in bytes.chunks(chunk_size.max(1)) {
            if let Some(event) = detector.feed(chunk) {
                return Some(event);
            }
        }
        None
    }

    #[test]
    fn github_piped_real_capture_is_detected() {
        let mut detector = AuthCodeDetector::new();
        let event = detector.feed(GH_PIPED_REAL.as_bytes()).expect("must detect the real gh device-code prompt");
        assert_eq!(
            event,
            DetectedPrompt {
                provider: DetectedProvider::Github,
                user_code: "F1FE-DF1C".to_string(),
                verification_uri: "https://github.com/login/device".to_string(),
            }
        );
    }

    #[test]
    fn github_real_pty_capture_with_ansi_color_codes_is_detected() {
        let mut detector = AuthCodeDetector::new();
        let event = detector.feed(GH_PTY_REAL_RAW).expect("must detect through real ANSI escape sequences");
        assert_eq!(
            event,
            DetectedPrompt {
                provider: DetectedProvider::Github,
                user_code: "21F9-D986".to_string(),
                verification_uri: "https://github.com/login/device".to_string(),
            }
        );
    }

    #[test]
    fn github_detection_survives_the_marker_and_code_split_across_reads() {
        for chunk_size in [1, 3, 7, 16, 64] {
            let event = feed_in_chunks_of(GH_PIPED_REAL.as_bytes(), chunk_size);
            assert_eq!(
                event,
                Some(DetectedPrompt {
                    provider: DetectedProvider::Github,
                    user_code: "F1FE-DF1C".to_string(),
                    verification_uri: "https://github.com/login/device".to_string(),
                }),
                "failed to detect with chunk_size={chunk_size}"
            );
        }
    }

    #[test]
    fn aws_print_only_handler_output_is_detected() {
        let text = aws_print_only_handler_output(
            "https://device.sso.us-east-1.amazonaws.com/",
            "ABCD-WXYZ",
            "https://device.sso.us-east-1.amazonaws.com/?user_code=ABCD-WXYZ",
        );
        let mut detector = AuthCodeDetector::new();
        let event = detector.feed(text.as_bytes()).expect("must detect the aws PrintOnlyHandler prompt");
        assert_eq!(
            event,
            DetectedPrompt {
                provider: DetectedProvider::AwsSso,
                user_code: "ABCD-WXYZ".to_string(),
                verification_uri: "https://device.sso.us-east-1.amazonaws.com/".to_string(),
            }
        );
    }

    #[test]
    fn aws_open_browser_handler_output_is_detected_and_does_not_capture_the_complete_uri() {
        let text = aws_open_browser_handler_output("https://device.sso.us-east-1.amazonaws.com/", "QRST-1234");
        let mut detector = AuthCodeDetector::new();
        let event = detector.feed(text.as_bytes()).unwrap();
        assert_eq!(
            event,
            DetectedPrompt {
                provider: DetectedProvider::AwsSso,
                user_code: "QRST-1234".to_string(),
                verification_uri: "https://device.sso.us-east-1.amazonaws.com/".to_string(),
            }
        );
    }

    #[test]
    fn real_aws_error_output_is_not_a_false_positive() {
        let mut detector = AuthCodeDetector::new();
        assert_eq!(detector.feed(AWS_REAL_INVALID_START_URL_ERROR.as_bytes()), None);
    }

    #[test]
    fn azure_device_code_message_is_detected() {
        let text = az_device_code_output("BQFQAAT9V");
        let mut detector = AuthCodeDetector::new();
        let event = detector.feed(text.as_bytes()).unwrap();
        assert_eq!(
            event,
            DetectedPrompt {
                provider: DetectedProvider::Azure,
                user_code: "BQFQAAT9V".to_string(),
                verification_uri: "https://microsoft.com/devicelogin".to_string(),
            }
        );
    }

    /// A realistic reconstruction of `gcloud auth login --no-launch-browser`'s
    /// real, currently-documented output (see the block comment above
    /// [`AuthCodeDetector`]'s `feed` dispatch chain for the research this is
    /// based on — no `gcloud` binary was available in this environment to
    /// capture it live). The load-bearing property under test isn't the
    /// exact wording, it's the *shape*: `gcloud` never prints a short code
    /// here at all, only a prompt asking the user to type one in — so no
    /// pattern tuned against this module's `(provider, user_code,
    /// verification_uri)` shape could ever legitimately match it.
    const GCP_NO_LAUNCH_BROWSER_REALISTIC: &str = "Go to the following link in your browser:\n\n    https://accounts.google.com/o/oauth2/auth?response_type=code&client_id=32555940559.apps.googleusercontent.com&redirect_uri=urn%3Aietf%3Awg%3Aoauth%3A2.0%3Aoob&scope=openid+https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fuserinfo.email&state=abc123\n\nEnter authorization code: ";

    /// A realistic reconstruction of `gcloud auth login --no-browser`'s
    /// current remote-bootstrap output — the *other* real headless flow
    /// `gcloud` ships, structurally different again (a full command to run
    /// on another machine, not a code), same conclusion: no short code
    /// anywhere in it.
    const GCP_NO_BROWSER_REMOTE_BOOTSTRAP_REALISTIC: &str = "To sign in, run this command on a machine with a web browser:\n\n  gcloud auth login --remote-bootstrap=\"https://accounts.google.com/o/oauth2/auth?response_type=code%3A&client_id=32555940559.apps.googleusercontent.com&redirect_uri=https%3A%2F%2Fsdk.cloud.google.com%2Fauthcode.html&scope=openid&state=xyz789\"\n\nEnter the output of the above command: ";

    #[test]
    fn gcp_realistic_no_launch_browser_output_is_not_a_false_positive() {
        // Proves the "gcp isn't representable in the current event shape"
        // conclusion is actually enforced, not just asserted in a comment:
        // this module ships no gcp matcher, so this must not fire.
        let mut detector = AuthCodeDetector::new();
        assert_eq!(detector.feed(GCP_NO_LAUNCH_BROWSER_REALISTIC.as_bytes()), None);
    }

    #[test]
    fn gcp_realistic_no_browser_remote_bootstrap_output_is_not_a_false_positive() {
        let mut detector = AuthCodeDetector::new();
        assert_eq!(detector.feed(GCP_NO_BROWSER_REMOTE_BOOTSTRAP_REALISTIC.as_bytes()), None);
    }

    #[test]
    fn ordinary_shell_output_never_matches_any_provider() {
        let samples = [
            "total 12\ndrwxr-xr-x  3 user user 4096 Aug 14 00:00 .\n-rw-r--r--  1 user user   42 Aug 14 00:00 README.md\n",
            "$ git status\nOn branch main\nnothing to commit, working tree clean\n",
            "npm WARN deprecated something@1.0.0: use something-else instead\n",
            "Then enter the wrong thing entirely, not a code\n",
            "",
        ];
        for sample in samples {
            let mut detector = AuthCodeDetector::new();
            assert_eq!(detector.feed(sample.as_bytes()), None, "unexpected match on: {sample:?}");
        }
    }

    #[test]
    fn a_different_kind_of_auth_failure_does_not_false_positive() {
        // gh's own "not logged in" status output — a real auth-adjacent
        // message that must not be confused with a device-code prompt.
        let gh_status = "gh auth status\nX Not logged in to github.com\n";
        // A generic OAuth error unrelated to any of the four providers'
        // real device-code phrasing.
        let generic_oauth_error = "Error: invalid_grant: Token has been expired or revoked.\n";
        for sample in [gh_status, generic_oauth_error, AWS_REAL_INVALID_START_URL_ERROR] {
            let mut detector = AuthCodeDetector::new();
            assert_eq!(detector.feed(sample.as_bytes()), None, "unexpected match on: {sample:?}");
        }
    }

    #[test]
    fn partial_output_missing_the_verification_uri_does_not_fire() {
        let mut detector = AuthCodeDetector::new();
        // Only the code line has arrived so far; the CLI hasn't printed
        // the URL line yet (a real, reachable interleaving — the process
        // could be scheduled out between the two `print`/`uni_print`
        // calls, or the pty reader could simply have read up to here).
        assert_eq!(detector.feed(b"! First copy your one-time code: F1FE-DF1C\n"), None);
        // The rest arrives in a later read; only *now* must it fire.
        let event = detector.feed(b"Open this URL to continue in your web browser: https://github.com/login/device\n").unwrap();
        assert_eq!(
            event,
            DetectedPrompt {
                provider: DetectedProvider::Github,
                user_code: "F1FE-DF1C".to_string(),
                verification_uri: "https://github.com/login/device".to_string(),
            }
        );
    }

    #[test]
    fn a_code_cut_off_mid_token_by_a_chunk_boundary_does_not_fire_early() {
        let mut detector = AuthCodeDetector::new();
        // "F1FE-DF1C" truncated to "F1FE-DF" with no trailing whitespace
        // yet — take_token must refuse to guess this is the complete code.
        assert_eq!(detector.feed(b"! First copy your one-time code: F1FE-DF"), None);
        let event = detector
            .feed(b"1C\nOpen this URL to continue in your web browser: https://github.com/login/device\n")
            .unwrap();
        assert_eq!(
            event,
            DetectedPrompt {
                provider: DetectedProvider::Github,
                user_code: "F1FE-DF1C".to_string(),
                verification_uri: "https://github.com/login/device".to_string(),
            }
        );
    }

    #[test]
    fn detecting_twice_for_the_same_code_only_fires_once() {
        let mut detector = AuthCodeDetector::new();
        assert!(detector.feed(GH_PIPED_REAL.as_bytes()).is_some());
        // The same prompt text is still sitting in the pty's scrollback
        // and gets read again (e.g. the user's terminal redraws, or a
        // second small read happens to re-cover the same bytes) — must
        // not re-fire for an identical (provider, user_code).
        assert_eq!(detector.feed(GH_PIPED_REAL.as_bytes()), None);
        assert_eq!(detector.feed(GH_PIPED_REAL.as_bytes()), None);
    }

    #[test]
    fn a_genuinely_new_code_after_an_already_fired_one_still_fires() {
        let mut detector = AuthCodeDetector::new();
        assert!(detector.feed(GH_PIPED_REAL.as_bytes()).is_some());
        let second_login = "! First copy your one-time code: 9999-ZZZZ\nOpen this URL to continue in your web browser: https://github.com/login/device\n";
        let event = detector.feed(second_login.as_bytes()).unwrap();
        assert_eq!(
            event,
            DetectedPrompt {
                provider: DetectedProvider::Github,
                user_code: "9999-ZZZZ".to_string(),
                verification_uri: "https://github.com/login/device".to_string(),
            }
        );
    }

    /// The no-leakage requirement, proven directly: a realistic-looking
    /// secret sitting elsewhere in the very same output stream — before
    /// and after the real device-code prompt, on the same lines a naive
    /// "capture the whole match region" implementation might have grabbed
    /// — must never end up in the emitted event's fields.
    #[test]
    fn embedded_fake_secrets_elsewhere_in_the_stream_never_reach_the_event() {
        let poisoned = format!(
            "noise AWS_SECRET_ACCESS_KEY=wJalrFAKESECRETKEYEXAMPLEbPxRfiCYEXAMPLEKEY more noise\n{GH_PIPED_REAL}trailing GH_TOKEN=ghp_FAKEfakeFAKEfakeFAKEfakeFAKEfake1234\n"
        );
        let mut detector = AuthCodeDetector::new();
        let event = detector.feed(poisoned.as_bytes()).expect("the real prompt embedded in the poisoned stream must still be found");
        let DetectedPrompt { provider, user_code, verification_uri } = &event;
        assert_eq!(*provider, DetectedProvider::Github);
        assert_eq!(user_code, "F1FE-DF1C");
        assert_eq!(verification_uri, "https://github.com/login/device");

        // Belt and braces: build the real wire event this `DetectedPrompt`
        // would end up in (`resources.rs::event_for_pattern_a_detection`'s
        // job in production — reconstructed inline here so this module's
        // own tests don't need a dependency on `resources.rs`) and confirm
        // the fake secrets are nowhere in its serialized or Debug form —
        // not truncated into a field, not appended, nothing.
        let wire_event = choosh_protocol::relay::WireAgentEvent::ResourceReauthRequired {
            resource_id: "detected-github".to_string(),
            display_name: "GitHub".to_string(),
            resource_kind: "github".to_string(),
            pattern: choosh_protocol::relay::WireResourcePattern::A,
            verification_uri: Some(verification_uri.clone()),
            user_code: Some(user_code.clone()),
            fetch_instructions: None,
            mobile_profile: choosh_protocol::relay::WireMobileProfile::Ask,
        };
        let json = serde_json::to_string(&wire_event).unwrap();
        assert!(!json.contains("AWS_SECRET_ACCESS_KEY"));
        assert!(!json.contains("wJalrFAKESECRETKEYEXAMPLEbPxRfiCYEXAMPLEKEY"));
        assert!(!json.contains("GH_TOKEN"));
        assert!(!json.contains("ghp_FAKEfakeFAKEfakeFAKEfakeFAKEfake1234"));
        assert!(!json.contains("noise"));
        // And the Debug representation too, in case a future change logs
        // the event directly rather than its serialized form.
        let debug = format!("{wire_event:?}");
        assert!(!debug.contains("AWS_SECRET_ACCESS_KEY"));
        assert!(!debug.contains("GH_TOKEN"));
    }

    #[test]
    fn strip_ansi_removes_csi_and_osc_sequences_and_normalizes_line_endings() {
        assert_eq!(strip_ansi_and_normalize("\x1b[0;33mhello\x1b[0m\r\nworld"), "hello\nworld");
        assert_eq!(strip_ansi_and_normalize("\x1b]11;?\x1b\\ok"), "ok");
        assert_eq!(strip_ansi_and_normalize("\x1b[6nplain"), "plain");
        assert_eq!(strip_ansi_and_normalize("no escapes here"), "no escapes here");
    }

    #[test]
    fn looks_like_device_code_rejects_obviously_wrong_shapes() {
        assert!(looks_like_device_code("F1FE-DF1C"));
        assert!(looks_like_device_code("BQFQAAT9V"));
        assert!(!looks_like_device_code("abc")); // too short
        assert!(!looks_like_device_code("this is not a code")); // contains spaces (never a single token anyway)
        assert!(!looks_like_device_code("----")); // no alphanumeric content at all
        assert!(!looks_like_device_code(&"A".repeat(64))); // absurdly long
    }
}
