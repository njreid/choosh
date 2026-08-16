//! The built-in `Resource` `kind` table, per
//! `docs/specs/resources-and-reauth.md`'s "Provider survey" and "Built-in
//! kinds" sections: seven pre-populated kinds (`aws-sso`, `aws-iam-key`,
//! `aws-sts-mfa`, `gcloud`, `firebase`, `github`, `azure`), each carrying
//! its interaction [`choosh_protocol::relay::WireResourcePattern`] and a
//! sensible default `reauth_command` — and, where the pattern needs it, a
//! default `fetch_instructions` (pattern c) or `resume_command_template`
//! (pattern d). `resources.rs`'s `ResourceRegistry::propose` validates an
//! incoming `resource_kind` against this table (the literal string
//! `"custom"` skips it entirely — see that module).
//!
//! This is *data*, not a hardcoded `match` per provider — the actual
//! generalization `resources-and-reauth.md` asks for over
//! `auth_detect.rs`'s original four-provider `match` chain. `detect_kind`
//! is the one place this table still points back at real, hardcoded Rust:
//! per this task's build note, only pattern a is ever passively detected,
//! and only by the three real, verified matchers `auth_detect.rs` already
//! ships (`github`/`aws-sso`/`azure` — not `gcloud`, which has no matcher
//! at all, per that module's own doc comment) — making those three fully
//! data-driven is explicitly out of scope here and left as the documented
//! future refinement `resources-and-reauth.md` flags.

use choosh_protocol::relay::WireResourcePattern;

/// One built-in `Resource` kind's fixed shape. Every field here is a
/// *default* a proposer/operator can layer a real value over at
/// declaration time (`ResourceRegistry::propose`/`confirm`) — this table
/// never appears on the wire directly.
pub struct ResourceKindDef {
    /// The exact `resource_kind` string this row matches — one of
    /// [`BUILTIN_KINDS`]'s seven values, never `"custom"`.
    pub kind: &'static str,
    /// Shown on the phone when a Resource of this kind is synthesized (a
    /// pattern-a passive detection with no matching registered Resource
    /// yet) or when a proposer didn't otherwise specify one — `resource.propose`
    /// always requires a real `display_name`, so this is a fallback only,
    /// never silently overriding a caller-supplied one.
    pub default_display_name: &'static str,
    pub pattern: WireResourcePattern,
    pub default_reauth_command: &'static str,
    /// Only meaningful for `pattern == C`.
    pub default_fetch_instructions: Option<&'static str>,
    /// Only meaningful for `pattern == D`. `{code}` is the substitution
    /// placeholder `resource_reauth.rs`'s `ResourceReauthComplete` handling
    /// replaces with the phone-supplied value.
    pub default_resume_command_template: Option<&'static str>,
    /// Only meaningful for `pattern == A`: which of `auth_detect.rs`'s real
    /// matchers (`"github"`, `"aws-sso"`, `"azure"`) recognizes this kind's
    /// device-code prompt in a pty's output. `None` for every kind whose
    /// pattern isn't `A` (nothing to detect passively) and for `gcloud`,
    /// which is pattern b, not a — see `auth_detect.rs`'s own doc comment
    /// for why no matcher exists for it at all.
    pub detect_kind: Option<&'static str>,
}

/// `docs/specs/resources-and-reauth.md`'s full "Provider survey" table,
/// verification status included in each row's own doc reference — see that
/// spec section for the live-capture/research-only distinction per kind.
pub const BUILTIN_KINDS: &[ResourceKindDef] = &[
    // Pattern A, verified against real, installed `aws-cli 2.35.17` source
    // (`auth_detect.rs::detect_aws`'s own doc comment has the full detail).
    ResourceKindDef {
        kind: "aws-sso",
        default_display_name: "AWS SSO",
        pattern: WireResourcePattern::A,
        default_reauth_command: "aws sso login --no-browser",
        default_fetch_instructions: None,
        default_resume_command_template: None,
        detect_kind: Some("aws-sso"),
    },
    // Pattern C, captured live (`aws-cli 2.35.17`) — four sequential
    // prompts, no URL, no code; the values must already exist in the AWS
    // Console (IAM -> Users -> Security credentials).
    ResourceKindDef {
        kind: "aws-iam-key",
        default_display_name: "AWS IAM Access Key",
        pattern: WireResourcePattern::C,
        default_reauth_command: "aws configure",
        default_fetch_instructions: Some(
            "Create or rotate an Access Key ID and Secret Access Key under IAM -> Users -> Security credentials in the AWS Console (https://console.aws.amazon.com/iam/), then provide the Secret Access Key here.",
        ),
        default_resume_command_template: None,
        detect_kind: None,
    },
    // Pattern C/D hybrid per the survey (not interactive at all —
    // `--token-code` is a plain argument, sourced from an authenticator
    // app, not a website): modeled here as D, since "run a fresh,
    // non-interactive command once the value is in hand" is exactly
    // `resource_reauth.rs`'s pattern-d handling, and there is no live
    // process to paste a value back into the way b/c need.
    ResourceKindDef {
        kind: "aws-sts-mfa",
        default_display_name: "AWS STS Session Token (MFA)",
        pattern: WireResourcePattern::D,
        default_reauth_command: "aws sts get-session-token --serial-number <mfa-device-arn>",
        default_fetch_instructions: None,
        default_resume_command_template: Some("aws sts get-session-token --serial-number <mfa-device-arn> --token-code {code}"),
        detect_kind: None,
    },
    // Pattern B, researched (no live `gcloud` binary — see
    // `resources-and-reauth.md`'s own flagged caveat). `--no-launch-browser`
    // is the sub-flow modeled here (a single URL, a single paste-back
    // prompt) rather than `--no-browser`'s remote-bootstrap variant, which
    // is structurally the same shape but a longer command instead of a
    // short code — either way the phone-supplied value must reach this
    // still-blocked process's stdin.
    ResourceKindDef {
        kind: "gcloud",
        default_display_name: "gcloud",
        pattern: WireResourcePattern::B,
        default_reauth_command: "gcloud auth login --no-launch-browser",
        default_fetch_instructions: None,
        default_resume_command_template: None,
        detect_kind: None,
    },
    // Pattern D, captured live (`firebase-tools 15.27.0`): the CLI does
    // keep polling in the background after printing its URL (behaving like
    // pattern a in the common case), but the printed `firebase login
    // <authorizationCode>` fallback is the one path guaranteed to work
    // regardless of the devhost's own network posture — see the spec's
    // Firebase section for why that's the one worth building against.
    ResourceKindDef {
        kind: "firebase",
        default_display_name: "Firebase CLI",
        pattern: WireResourcePattern::D,
        default_reauth_command: "firebase login --no-localhost",
        default_fetch_instructions: None,
        default_resume_command_template: Some("firebase login {code}"),
        detect_kind: None,
    },
    // Pattern A, verified against real, installed `gh 2.96.0`, both piped
    // and through a real pty (`auth_detect.rs::detect_github`).
    ResourceKindDef {
        kind: "github",
        default_display_name: "GitHub",
        pattern: WireResourcePattern::A,
        default_reauth_command: "gh auth login --web",
        default_fetch_instructions: None,
        default_resume_command_template: None,
        detect_kind: Some("github"),
    },
    // Pattern A, well-corroborated but not live-verified (`az` not
    // installed — `auth_detect.rs::detect_azure`'s own doc comment has the
    // full corroboration detail: this is Entra ID's own OAuth device-code
    // response text, not `az`-authored).
    ResourceKindDef {
        kind: "azure",
        default_display_name: "Azure CLI",
        pattern: WireResourcePattern::A,
        default_reauth_command: "az login",
        default_fetch_instructions: None,
        default_resume_command_template: None,
        detect_kind: Some("azure"),
    },
];

/// Looks up `kind` in [`BUILTIN_KINDS`] — `None` for `"custom"` and for
/// anything not in the table (both are the caller's job to distinguish;
/// see `ResourceRegistry::propose`).
#[must_use]
pub fn lookup(kind: &str) -> Option<&'static ResourceKindDef> {
    BUILTIN_KINDS.iter().find(|def| def.kind == kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_built_in_kind_is_unique_and_round_trips_through_lookup() {
        let mut seen = std::collections::HashSet::new();
        for def in BUILTIN_KINDS {
            assert!(seen.insert(def.kind), "duplicate built-in kind {:?}", def.kind);
            assert_eq!(lookup(def.kind).map(|d| d.kind), Some(def.kind));
        }
        assert_eq!(BUILTIN_KINDS.len(), 7, "resources-and-reauth.md's survey names exactly seven built-in kinds");
    }

    #[test]
    fn lookup_of_custom_and_unknown_kinds_is_none() {
        assert!(lookup("custom").is_none());
        assert!(lookup("twilio").is_none(), "twilio is explicitly a 'representative custom example', not a built-in row");
    }

    #[test]
    fn every_pattern_c_row_has_fetch_instructions_and_every_pattern_d_row_has_a_resume_template() {
        for def in BUILTIN_KINDS {
            match def.pattern {
                WireResourcePattern::C => assert!(def.default_fetch_instructions.is_some(), "{:?} is pattern c with no fetch_instructions", def.kind),
                WireResourcePattern::D => assert!(def.default_resume_command_template.is_some(), "{:?} is pattern d with no resume_command_template", def.kind),
                _ => {}
            }
        }
    }

    #[test]
    fn only_the_three_real_auth_detect_matchers_have_a_detect_kind() {
        let with_detect: Vec<&str> = BUILTIN_KINDS.iter().filter_map(|d| d.detect_kind).collect();
        let mut sorted = with_detect.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec!["aws-sso", "azure", "github"]);
    }
}
