//! `docs/specs/resources-and-reauth.md`'s `Resource` entity: a persistent,
//! devhost-scoped registry of named, typed references to something that
//! occasionally needs a human to complete a re-auth step (or is otherwise
//! external infrastructure an agent can be pointed at) — mirrors
//! `registry.rs`'s JSON-backed load/save discipline for the same reason
//! that module gives ("a `choosh-hostd` restart shouldn't forget what's
//! registered").
//!
//! **Confirmed Resources vs. pending proposals.** Per the spec's
//! "Agent-declared resources" decision, a proposal is scratch state until a
//! human approves it — [`ResourceRegistry`] holds the two pools separately
//! ([`ResourceRegistry::propose`]/[`ResourceRegistry::confirm`]), and only
//! the confirmed pool is ever persisted to disk or returned by
//! [`ResourceRegistry::list`]. Pending proposals are deliberately
//! in-memory-only (never written to the JSON file) — the same posture
//! `registry.rs`'s `reserved_workspace_names` takes for other short-lived,
//! single-in-flight-call state: if `choosh-hostd` restarts before a human
//! answers, the pending question is gone anyway (the `input_required`
//! round trip that surfaced it doesn't survive a restart either), so
//! persisting the proposal itself would just be a corpse in the file.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use choosh_protocol::host_rpc::WireResource;
use choosh_protocol::relay::{WireAgentEvent, WireMobileProfile, WireResourcePattern};
use serde::{Deserialize, Serialize};

use crate::auth_detect::DetectedPrompt;
use crate::resource_kinds;

/// A confirmed, listed Resource — the Rust-side superset of
/// [`WireResource`]: everything on the wire type, plus the
/// orchestration-only fields `resource_reauth.rs`'s managed-subprocess
/// runner needs and that must never reach the phone (a shell command line
/// is not something `resources-and-reauth.md`'s "no token, credential, or
/// session identifier" rule is about, but it's still not display data —
/// `to_wire` is the one, explicit place this gets narrowed for the wire).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Resource {
    pub resource_id: String,
    pub display_name: String,
    pub resource_kind: String,
    pub pattern: Option<WireResourcePattern>,
    pub mobile_profile: WireMobileProfile,
    pub created_by: String,
    pub last_used_at: Option<String>,
    pub last_verified_at: Option<String>,
    /// The command `resource_reauth.rs` spawns for patterns b/c (and the
    /// command a/b/c's `resource.list` entry point runs by hand, for a/b
    /// that's a phone-triggered convenience, not this crate's concern).
    /// `None` only for a non-reauth Resource (`pattern: None`) with no
    /// command supplied at declaration time.
    pub reauth_command: Option<String>,
    /// Pattern-d only: the fresh, non-interactive command
    /// `resource_reauth.rs::ResourceReauthComplete` runs once a value is in
    /// hand, with `{code}` substituted for it.
    pub resume_command_template: Option<String>,
    /// Pattern-c only: human-readable "go get X from Y" text, shown on the
    /// phone verbatim as `WireAgentEvent::ResourceReauthRequired`'s
    /// `fetch_instructions`.
    pub fetch_instructions: Option<String>,
    /// Pattern-a only: which of `auth_detect.rs`'s three real matchers
    /// (`"github"`, `"aws-sso"`, `"azure"`) this Resource's passive
    /// detection should be attributed to. `None` for every other pattern,
    /// and for a pattern-a Resource declared under a `resource_kind` this
    /// crate has no matcher for (a `"custom"` pattern-a Resource, say —
    /// legal to declare, just never passively detected).
    pub detect_kind: Option<String>,
}

impl Resource {
    #[must_use]
    pub fn to_wire(&self) -> WireResource {
        WireResource {
            resource_id: self.resource_id.clone(),
            display_name: self.display_name.clone(),
            resource_kind: self.resource_kind.clone(),
            pattern: self.pattern,
            mobile_profile: self.mobile_profile,
            created_by: self.created_by.clone(),
            last_used_at: self.last_used_at.clone(),
            last_verified_at: self.last_verified_at.clone(),
        }
    }
}

/// An agent's (or operator's) not-yet-approved `resource.propose` request —
/// the same shape as [`Resource`] minus the persistence-only metadata
/// (`last_used_at`/`last_verified_at`, which don't exist until something is
/// actually confirmed) and minus its own `resource_id` (it's keyed by a
/// separate, freshly-minted proposal id — see [`ResourceRegistry::propose`]'s
/// doc comment for why that id is deliberately not reused as the eventual
/// Resource's `resource_id`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingProposal {
    pub proposal_id: String,
    pub display_name: String,
    pub resource_kind: String,
    pub pattern: Option<WireResourcePattern>,
    pub reauth_command: Option<String>,
    pub mobile_profile: WireMobileProfile,
    pub created_by: String,
}

#[derive(Debug)]
pub enum ResourceRegistryError {
    Io(io::Error),
    Corrupt(String),
    /// `resource_kind` is neither `"custom"` nor one of
    /// [`resource_kinds::BUILTIN_KINDS`].
    UnknownKind(String),
    /// `resource_kind == "custom"` and `pattern == Some(WireResourcePattern::D)`:
    /// rejected, not silently downgraded — see [`ResourceRegistry::propose`]'s
    /// doc comment for why pattern d specifically needs a real
    /// `resume_command_template`, which nothing on the wire lets a custom
    /// proposal supply.
    CustomPatternDUnsupported,
    /// `resource.confirm`'s `resource_id` doesn't name a pending proposal
    /// (already resolved, or never existed).
    ProposalNotFound(String),
    /// `resource_id` doesn't name a confirmed, registered Resource.
    ResourceNotFound(String),
}

impl std::fmt::Display for ResourceRegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "resource registry file I/O error: {error}"),
            Self::Corrupt(reason) => write!(f, "resource registry file is corrupt: {reason}"),
            Self::UnknownKind(kind) => write!(f, "{kind:?} is not a built-in resource kind and is not \"custom\""),
            Self::CustomPatternDUnsupported => write!(
                f,
                "a \"custom\" resource_kind cannot use pattern \"d\" — no resume_command_template can be supplied for a custom proposal"
            ),
            Self::ProposalNotFound(id) => write!(f, "no pending resource proposal {id:?}"),
            Self::ResourceNotFound(id) => write!(f, "no registered resource {id:?}"),
        }
    }
}

impl std::error::Error for ResourceRegistryError {}

impl From<io::Error> for ResourceRegistryError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
struct ResourceRegistryState {
    resources: Vec<Resource>,
}

/// In-memory registry, loaded from and saved back to a single JSON file —
/// confirmed Resources only; see this module's own doc comment for why
/// pending proposals never touch disk. Not safe for concurrent mutation
/// from multiple processes, same single-`choosh-hostd`-per-devhost
/// reasoning `registry.rs::Registry` documents on itself.
pub struct ResourceRegistry {
    path: PathBuf,
    state: ResourceRegistryState,
    pending: HashMap<String, PendingProposal>,
}

impl ResourceRegistry {
    /// # Errors
    ///
    /// Returns [`ResourceRegistryError::Corrupt`] for a present, malformed
    /// file — same "a corrupt file is a hard error, not silently treated as
    /// empty" posture `registry.rs::Registry::load` takes.
    pub fn load(path: &Path) -> Result<Self, ResourceRegistryError> {
        let state = match fs::read(path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|error| ResourceRegistryError::Corrupt(format!("invalid JSON: {error}")))?
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => ResourceRegistryState::default(),
            Err(error) => return Err(ResourceRegistryError::Io(error)),
        };
        Ok(Self { path: path.to_path_buf(), state, pending: HashMap::new() })
    }

    /// The default per-devhost resource file location:
    /// `<config dir>/choosh-hostd/resources.json` — a sibling of
    /// `registry.rs::Registry::default_path`'s `workspaces.json`, same
    /// config directory, distinct file (Resources are devhost-scoped, not
    /// Workspace-scoped, but both live under this one devhost's config
    /// root).
    ///
    /// # Errors
    ///
    /// Returns an I/O error if no config directory can be determined.
    pub fn default_path() -> Result<PathBuf, ResourceRegistryError> {
        let dirs = directories::ProjectDirs::from("ai", "choosh", "hostd")
            .ok_or_else(|| ResourceRegistryError::Io(io::Error::new(io::ErrorKind::NotFound, "no config directory for choosh-hostd")))?;
        Ok(dirs.config_dir().join("resources.json"))
    }

    fn save(&self) -> Result<(), ResourceRegistryError> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| ResourceRegistryError::Io(io::Error::new(io::ErrorKind::InvalidInput, "resource registry path has no parent directory")))?;
        fs::create_dir_all(parent)?;
        let tmp_path = parent.join(format!(
            ".{}.tmp-{}",
            self.path.file_name().and_then(|n| n.to_str()).unwrap_or("resources.json"),
            std::process::id()
        ));
        let json = serde_json::to_vec_pretty(&self.state)
            .map_err(|error| ResourceRegistryError::Io(io::Error::new(io::ErrorKind::InvalidData, error)))?;
        fs::write(&tmp_path, &json)?;
        fs::rename(&tmp_path, &self.path)?;
        Ok(())
    }

    /// Every confirmed, registered Resource — `resource.list`'s source of
    /// rows. Never includes a pending, unconfirmed proposal, per the
    /// confirmation-gate decision.
    #[must_use]
    pub fn list(&self) -> &[Resource] {
        &self.state.resources
    }

    #[must_use]
    pub fn get(&self, resource_id: &str) -> Option<&Resource> {
        self.state.resources.iter().find(|r| r.resource_id == resource_id)
    }

    /// The first confirmed, registered pattern-a Resource whose
    /// `resource_kind` matches `kind` (one of `auth_detect.rs`'s
    /// `DetectedProvider::resource_kind()` strings) — used by
    /// [`event_for_pattern_a_detection`] to attribute a passive detection
    /// to a real, operator-declared Resource when one exists.
    #[must_use]
    fn find_confirmed_pattern_a(&self, kind: &str) -> Option<&Resource> {
        self.state.resources.iter().find(|r| r.pattern == Some(WireResourcePattern::A) && r.resource_kind == kind)
    }

    /// Stores `PendingProposal` scratch state and returns its freshly-minted
    /// id (returned to the caller as `ResourceProposeOk::resource_id`,
    /// per that response's own doc comment — it names the *pending*
    /// proposal, not a real Resource yet). Deliberately a fresh id here
    /// rather than pre-minting the eventual Resource's `resource_id`: an
    /// `approve: false` `resource.confirm` never produces a Resource at
    /// all, so reserving a `res-...` id up front for something that might
    /// never exist would be a needless, confusing pre-commitment — the
    /// real `resource_id` is only minted in [`Self::confirm`], once
    /// approval is certain.
    ///
    /// # Errors
    ///
    /// [`ResourceRegistryError::UnknownKind`] if `resource_kind` is neither
    /// `"custom"` nor a row in [`resource_kinds::BUILTIN_KINDS`].
    /// [`ResourceRegistryError::CustomPatternDUnsupported`] if
    /// `resource_kind == "custom"` and `pattern == Some(WireResourcePattern::D)` —
    /// see that variant's own doc comment for why.
    pub fn propose(
        &mut self,
        display_name: String,
        resource_kind: String,
        pattern: Option<WireResourcePattern>,
        reauth_command: Option<String>,
        mobile_profile: WireMobileProfile,
        created_by: String,
    ) -> Result<String, ResourceRegistryError> {
        let resolved_pattern = validate_kind_and_pattern(&resource_kind, pattern)?;
        let proposal_id = format!("resprop-{}", uuid::Uuid::new_v4());
        self.pending.insert(
            proposal_id.clone(),
            PendingProposal { proposal_id: proposal_id.clone(), display_name, resource_kind, pattern: resolved_pattern, reauth_command, mobile_profile, created_by },
        );
        Ok(proposal_id)
    }

    /// Resolves a pending proposal: `approve: true` mints and persists a
    /// real Resource (built-in defaults for `fetch_instructions`/
    /// `resume_command_template`/`detect_kind` merged in per
    /// `resource_kinds.rs`, see [`resource_from_proposal`]) and returns it;
    /// `approve: false` simply drops the proposal and returns `None` — it
    /// never becomes a listed Resource, per the spec's confirmation-gate
    /// decision.
    ///
    /// # Errors
    ///
    /// [`ResourceRegistryError::ProposalNotFound`] if `proposal_id` doesn't
    /// name a pending proposal (already resolved, or never existed — both
    /// read the same to a caller, same as `registry.rs`'s reservation
    /// helpers being idempotent-on-absence, except here a *second*
    /// `resource.confirm` for the same id is a genuine error, not a no-op:
    /// unlike a reservation release, resolving a proposal twice would
    /// either double-mint a Resource or double-report a no-op discard,
    /// neither of which a caller should be able to trigger by retrying).
    pub fn confirm(&mut self, proposal_id: &str, approve: bool) -> Result<Option<Resource>, ResourceRegistryError> {
        let proposal = self.pending.remove(proposal_id).ok_or_else(|| ResourceRegistryError::ProposalNotFound(proposal_id.to_string()))?;
        if !approve {
            return Ok(None);
        }
        let resource = resource_from_proposal(proposal);
        self.state.resources.push(resource.clone());
        self.save()?;
        Ok(Some(resource))
    }

    /// Records a successful `resource.reauth_complete` — updates both
    /// `last_used_at` and `last_verified_at` to `at` and persists. A no-op
    /// (not an error) if `resource_id` isn't registered: `rpc.rs`'s
    /// `handle_resource_reauth_complete` already separately resolves
    /// `resource_id` via [`Self::get`] before ever reaching this call, so a
    /// second, redundant not-found path here would just be dead code —
    /// same "best-effort liveness update" posture
    /// `registry.rs::Registry::record_event` documents on itself.
    pub fn mark_verified(&mut self, resource_id: &str, at: String) {
        if let Some(resource) = self.state.resources.iter_mut().find(|r| r.resource_id == resource_id) {
            resource.last_used_at = Some(at.clone());
            resource.last_verified_at = Some(at);
            let _ = self.save();
        }
    }
}

/// Shared by [`ResourceRegistry::propose`] (validation only) — checked
/// again, not just at propose time, would be redundant here since
/// `confirm` re-derives everything from the already-validated
/// `PendingProposal` rather than re-validating.
///
/// For a built-in `resource_kind`, the caller-supplied `pattern` is
/// ignored entirely and the table's fixed pattern wins — a built-in kind's
/// interaction shape is a documented, verified fact about that provider
/// (`resources-and-reauth.md`'s survey), not something a proposer should
/// be able to override. For `"custom"`, `pattern` is returned unchanged
/// (`None` is legal — a non-reauth Resource, e.g. a second test host).
fn validate_kind_and_pattern(resource_kind: &str, pattern: Option<WireResourcePattern>) -> Result<Option<WireResourcePattern>, ResourceRegistryError> {
    if resource_kind == "custom" {
        if pattern == Some(WireResourcePattern::D) {
            return Err(ResourceRegistryError::CustomPatternDUnsupported);
        }
        return Ok(pattern);
    }
    match resource_kinds::lookup(resource_kind) {
        Some(def) => Ok(Some(def.pattern)),
        None => Err(ResourceRegistryError::UnknownKind(resource_kind.to_string())),
    }
}

/// Builds the real, persisted [`Resource`] a confirmed [`PendingProposal`]
/// becomes — the one place built-in defaults (`fetch_instructions`,
/// `resume_command_template`, `detect_kind`) get merged in for a built-in
/// `resource_kind`. For `"custom"`:
/// - pattern c gets a generic, synthesized `fetch_instructions` (there is
///   no wire field a custom proposal could have supplied one through —
///   `RpcRequest::ResourcePropose` only carries `reauth_command` — so a
///   custom pattern-c Resource would otherwise show the phone nothing at
///   all under "how do I get this value", which is worse than a generic
///   fallback).
/// - pattern d is unreachable here: rejected earlier, at `propose` time
///   (see [`ResourceRegistryError::CustomPatternDUnsupported`]).
/// - `detect_kind` is always `None` — passive pattern-a detection only
///   ever attributes to one of `auth_detect.rs`'s three real matchers,
///   never to a custom kind's name.
fn resource_from_proposal(proposal: PendingProposal) -> Resource {
    let resource_id = format!("res-{}", uuid::Uuid::new_v4());
    let builtin = resource_kinds::lookup(&proposal.resource_kind);
    let reauth_command = proposal.reauth_command.clone().or_else(|| builtin.map(|def| def.default_reauth_command.to_string()));
    let fetch_instructions = match (proposal.pattern, builtin) {
        (Some(WireResourcePattern::C), Some(def)) => def.default_fetch_instructions.map(str::to_string),
        (Some(WireResourcePattern::C), None) => {
            Some(format!("Provide the value \"{}\" needs; no additional fetch instructions were supplied when this resource was declared.", proposal.display_name))
        }
        _ => None,
    };
    let resume_command_template = match (proposal.pattern, builtin) {
        (Some(WireResourcePattern::D), Some(def)) => def.default_resume_command_template.map(str::to_string),
        _ => None,
    };
    let detect_kind = match (proposal.pattern, builtin) {
        (Some(WireResourcePattern::A), Some(def)) => def.detect_kind.map(str::to_string),
        _ => None,
    };
    Resource {
        resource_id,
        display_name: proposal.display_name,
        resource_kind: proposal.resource_kind,
        pattern: proposal.pattern,
        mobile_profile: proposal.mobile_profile,
        created_by: proposal.created_by,
        last_used_at: None,
        last_verified_at: None,
        reauth_command,
        resume_command_template,
        fetch_instructions,
        detect_kind,
    }
}

/// Turns a pattern-a [`DetectedPrompt`] (produced by
/// `auth_detect.rs::AuthCodeDetector::feed`, a pure text scanner with no
/// notion of Resources at all — see that module's doc comment) into the
/// real `WireAgentEvent::ResourceReauthRequired` the phone receives. If a
/// confirmed Resource of the matching kind is already registered, its real
/// `resource_id`/`display_name`/`mobile_profile` are used; otherwise a
/// transient one is synthesized (`resource_id: "detected-<kind>"`, a
/// built-in generic display name, `mobile_profile: Ask`) — per
/// `resources-and-reauth.md`'s "Agent-declared resources"/passive-detection
/// requirement that detection must keep working even before the operator
/// has explicitly registered anything.
#[must_use]
pub fn event_for_pattern_a_detection(registry: &ResourceRegistry, detection: &DetectedPrompt) -> WireAgentEvent {
    let kind = detection.provider.resource_kind();
    if let Some(resource) = registry.find_confirmed_pattern_a(kind) {
        return WireAgentEvent::ResourceReauthRequired {
            resource_id: resource.resource_id.clone(),
            display_name: resource.display_name.clone(),
            resource_kind: resource.resource_kind.clone(),
            pattern: WireResourcePattern::A,
            verification_uri: Some(detection.verification_uri.clone()),
            user_code: Some(detection.user_code.clone()),
            fetch_instructions: None,
            mobile_profile: resource.mobile_profile,
        };
    }
    let display_name = resource_kinds::lookup(kind).map_or_else(|| kind.to_string(), |def| def.default_display_name.to_string());
    WireAgentEvent::ResourceReauthRequired {
        resource_id: format!("detected-{kind}"),
        display_name,
        resource_kind: kind.to_string(),
        pattern: WireResourcePattern::A,
        verification_uri: Some(detection.verification_uri.clone()),
        user_code: Some(detection.user_code.clone()),
        fetch_instructions: None,
        mobile_profile: WireMobileProfile::Ask,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth_detect::DetectedProvider;

    fn temp_registry() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("resources.json");
        (dir, path)
    }

    #[test]
    fn missing_file_loads_empty() {
        let (_dir, path) = temp_registry();
        let registry = ResourceRegistry::load(&path).unwrap();
        assert!(registry.list().is_empty());
    }

    #[test]
    fn corrupt_file_is_a_hard_error() {
        let (_dir, path) = temp_registry();
        fs::write(&path, b"not json").unwrap();
        assert!(matches!(ResourceRegistry::load(&path), Err(ResourceRegistryError::Corrupt(_))));
    }

    #[test]
    fn confirmed_resource_round_trips_through_reload() {
        let (_dir, path) = temp_registry();
        let mut registry = ResourceRegistry::load(&path).unwrap();
        let proposal_id = registry
            .propose("Prod GitHub".to_string(), "github".to_string(), None, None, WireMobileProfile::Work, "operator".to_string())
            .unwrap();
        let resource = registry.confirm(&proposal_id, true).unwrap().expect("approved proposal must yield a Resource");
        assert_eq!(resource.pattern, Some(WireResourcePattern::A));
        assert_eq!(resource.reauth_command.as_deref(), Some("gh auth login --web"));

        let reloaded = ResourceRegistry::load(&path).unwrap();
        assert_eq!(reloaded.list().len(), 1);
        assert_eq!(reloaded.get(&resource.resource_id).unwrap().display_name, "Prod GitHub");
    }

    /// The core confirmation-gate regression test, per the spec's
    /// "Agent-declared resources" decision: a proposal must be invisible to
    /// `list()` before confirmation, and only appear after `approve: true`.
    #[test]
    fn a_pending_proposal_is_invisible_until_confirmed() {
        let (_dir, path) = temp_registry();
        let mut registry = ResourceRegistry::load(&path).unwrap();
        let proposal_id = registry
            .propose("Twilio".to_string(), "custom".to_string(), Some(WireResourcePattern::C), Some("twilio login".to_string()), WireMobileProfile::Ask, "agent:a1".to_string())
            .unwrap();
        assert!(registry.list().is_empty(), "an unconfirmed proposal must never appear in list()");

        let resource = registry.confirm(&proposal_id, true).unwrap().unwrap();
        assert_eq!(registry.list().len(), 1);
        assert_eq!(registry.list()[0].resource_id, resource.resource_id);
        assert_eq!(resource.created_by, "agent:a1");
        // A custom pattern-c resource with no explicit fetch_instructions
        // in the proposal still gets a usable fallback, not None.
        assert!(resource.fetch_instructions.is_some());
    }

    #[test]
    fn rejecting_a_proposal_never_lists_it_and_a_second_confirm_errors() {
        let (_dir, path) = temp_registry();
        let mut registry = ResourceRegistry::load(&path).unwrap();
        let proposal_id = registry
            .propose("Scratch host".to_string(), "custom".to_string(), None, Some("second EC2 test host".to_string()), WireMobileProfile::Ask, "operator".to_string())
            .unwrap();
        let result = registry.confirm(&proposal_id, false).unwrap();
        assert!(result.is_none());
        assert!(registry.list().is_empty());

        let second = registry.confirm(&proposal_id, true);
        assert!(matches!(second, Err(ResourceRegistryError::ProposalNotFound(id)) if id == proposal_id));
    }

    #[test]
    fn propose_rejects_an_unknown_kind() {
        let (_dir, path) = temp_registry();
        let mut registry = ResourceRegistry::load(&path).unwrap();
        let result = registry.propose("X".to_string(), "twilio".to_string(), Some(WireResourcePattern::C), None, WireMobileProfile::Ask, "operator".to_string());
        assert!(matches!(result, Err(ResourceRegistryError::UnknownKind(k)) if k == "twilio"));
    }

    #[test]
    fn propose_rejects_custom_pattern_d() {
        let (_dir, path) = temp_registry();
        let mut registry = ResourceRegistry::load(&path).unwrap();
        let result = registry.propose(
            "X".to_string(),
            "custom".to_string(),
            Some(WireResourcePattern::D),
            Some("resume {code}".to_string()),
            WireMobileProfile::Ask,
            "operator".to_string(),
        );
        assert!(matches!(result, Err(ResourceRegistryError::CustomPatternDUnsupported)));
    }

    #[test]
    fn a_built_in_kind_ignores_a_caller_supplied_pattern_override() {
        let (_dir, path) = temp_registry();
        let mut registry = ResourceRegistry::load(&path).unwrap();
        // github is always pattern a, no matter what the caller asks for.
        let proposal_id = registry
            .propose("GH".to_string(), "github".to_string(), Some(WireResourcePattern::C), None, WireMobileProfile::Ask, "operator".to_string())
            .unwrap();
        let resource = registry.confirm(&proposal_id, true).unwrap().unwrap();
        assert_eq!(resource.pattern, Some(WireResourcePattern::A));
    }

    #[test]
    fn a_firebase_proposal_gets_its_built_in_resume_command_template() {
        let (_dir, path) = temp_registry();
        let mut registry = ResourceRegistry::load(&path).unwrap();
        let proposal_id = registry.propose("Firebase".to_string(), "firebase".to_string(), None, None, WireMobileProfile::Ask, "operator".to_string()).unwrap();
        let resource = registry.confirm(&proposal_id, true).unwrap().unwrap();
        assert_eq!(resource.pattern, Some(WireResourcePattern::D));
        assert_eq!(resource.resume_command_template.as_deref(), Some("firebase login {code}"));
    }

    fn detection(provider: DetectedProvider, code: &str, uri: &str) -> DetectedPrompt {
        DetectedPrompt { provider, user_code: code.to_string(), verification_uri: uri.to_string() }
    }

    #[test]
    fn pattern_a_detection_synthesizes_a_transient_resource_when_none_is_registered() {
        let (_dir, path) = temp_registry();
        let registry = ResourceRegistry::load(&path).unwrap();
        let event = event_for_pattern_a_detection(&registry, &detection(DetectedProvider::Github, "ABCD-1234", "https://github.com/login/device"));
        let WireAgentEvent::ResourceReauthRequired { resource_id, display_name, resource_kind, pattern, verification_uri, user_code, fetch_instructions, mobile_profile } = event
        else {
            panic!("expected ResourceReauthRequired");
        };
        assert_eq!(resource_id, "detected-github");
        assert_eq!(display_name, "GitHub");
        assert_eq!(resource_kind, "github");
        assert_eq!(pattern, WireResourcePattern::A);
        assert_eq!(verification_uri.as_deref(), Some("https://github.com/login/device"));
        assert_eq!(user_code.as_deref(), Some("ABCD-1234"));
        assert_eq!(fetch_instructions, None);
        assert_eq!(mobile_profile, WireMobileProfile::Ask);
    }

    #[test]
    fn pattern_a_detection_attributes_to_a_real_registered_resource_when_one_matches() {
        let (_dir, path) = temp_registry();
        let mut registry = ResourceRegistry::load(&path).unwrap();
        let proposal_id = registry
            .propose("Prod AWS SSO".to_string(), "aws-sso".to_string(), None, None, WireMobileProfile::Work, "operator".to_string())
            .unwrap();
        let resource = registry.confirm(&proposal_id, true).unwrap().unwrap();

        let event = event_for_pattern_a_detection(&registry, &detection(DetectedProvider::AwsSso, "WXYZ-9999", "https://device.sso.us-east-1.amazonaws.com/"));
        let WireAgentEvent::ResourceReauthRequired { resource_id, display_name, mobile_profile, .. } = event else {
            panic!("expected ResourceReauthRequired");
        };
        assert_eq!(resource_id, resource.resource_id);
        assert_eq!(display_name, "Prod AWS SSO");
        assert_eq!(mobile_profile, WireMobileProfile::Work);
    }

    #[test]
    fn pattern_a_detection_never_attributes_to_a_resource_of_a_different_kind() {
        let (_dir, path) = temp_registry();
        let mut registry = ResourceRegistry::load(&path).unwrap();
        let proposal_id = registry.propose("Azure".to_string(), "azure".to_string(), None, None, WireMobileProfile::Ask, "operator".to_string()).unwrap();
        registry.confirm(&proposal_id, true).unwrap();

        // A github detection must not be attributed to the registered azure Resource.
        let event = event_for_pattern_a_detection(&registry, &detection(DetectedProvider::Github, "CODE-0000", "https://github.com/login/device"));
        let WireAgentEvent::ResourceReauthRequired { resource_id, .. } = event else {
            panic!("expected ResourceReauthRequired");
        };
        assert_eq!(resource_id, "detected-github");
    }
}
