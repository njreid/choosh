//! Filesystem-backed acceptance for the host deployment transaction.
//!
//! The fixture uses no process, network listener, wall clock, or shell. It
//! exercises the same narrow deployment capabilities that a future SSH upload
//! composition root will own.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use chooshd::upgrade::{
    ArtifactStore, DigestVerifier, HealthCheck, Release, UpgradeCoordinator, UpgradeFailure,
    UpgradeOutcome,
};
use sha2::{Digest, Sha256};

static FIXTURE_ID: AtomicUsize = AtomicUsize::new(1);

struct FixtureRoot(PathBuf);

impl FixtureRoot {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "choosh-upgrade-acceptance-{}-{}",
            std::process::id(),
            FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("unique fixture root");
        Self(root)
    }

    fn active(&self) -> PathBuf {
        self.0.join("active")
    }
}

impl Drop for FixtureRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone)]
struct Staged(PathBuf);

struct Previous(PathBuf);

struct LocalStore {
    root: PathBuf,
    corrupt_staging: bool,
    calls: Vec<&'static str>,
}

impl LocalStore {
    fn new(root: &FixtureRoot) -> Self {
        Self {
            root: root.0.clone(),
            corrupt_staging: false,
            calls: Vec::new(),
        }
    }

    fn active(&self) -> PathBuf {
        self.root.join("active")
    }

    fn previous(&self) -> PathBuf {
        self.root.join("previous")
    }
}

impl ArtifactStore for LocalStore {
    type Staged = Staged;
    type Previous = Previous;
    type Error = &'static str;

    fn stage(&mut self, version: &str, bytes: &[u8]) -> Result<Staged, Self::Error> {
        self.calls.push("stage");
        let path = self.root.join(format!("stage-{version}"));
        if path.exists() {
            return Err("stage_exists");
        }
        let staged = if self.corrupt_staging {
            b"corrupt"
        } else {
            bytes
        };
        fs::write(&path, staged).map_err(|_| "stage_write")?;
        Ok(Staged(path))
    }

    fn activate(&mut self, staged: Staged) -> Result<Previous, Self::Error> {
        self.calls.push("activate");
        let active = self.active();
        let previous = self.previous();
        if previous.exists() {
            return Err("previous_exists");
        }
        fs::rename(&active, &previous).map_err(|_| "preserve_active")?;
        fs::rename(staged.0, &active).map_err(|_| "activate_rename")?;
        Ok(Previous(previous))
    }

    fn rollback(&mut self, previous: Previous) -> Result<(), Self::Error> {
        self.calls.push("rollback");
        let failed = self.root.join("failed-activation");
        fs::rename(self.active(), failed).map_err(|_| "preserve_failed")?;
        fs::rename(previous.0, self.active()).map_err(|_| "restore_previous")
    }

    fn discard_staged(&mut self, staged: Staged) -> Result<(), Self::Error> {
        self.calls.push("discard");
        fs::remove_file(staged.0).map_err(|_| "discard")
    }
}

struct LocalDigest;

impl DigestVerifier<Staged> for LocalDigest {
    type Error = &'static str;

    fn verify_sha256(&mut self, staged: &Staged, expected: &[u8; 32]) -> Result<bool, Self::Error> {
        let bytes = fs::read(&staged.0).map_err(|_| "digest_read")?;
        Ok(sha256(&bytes) == *expected)
    }
}

struct ScriptedHealth {
    outcome: Result<bool, &'static str>,
    expected_version: &'static str,
    checked_versions: Vec<String>,
}

impl HealthCheck for ScriptedHealth {
    type Error = &'static str;

    fn healthy(&mut self, version: &str) -> Result<bool, Self::Error> {
        self.checked_versions.push(version.to_owned());
        if version != self.expected_version {
            return Err("unexpected_version");
        }
        self.outcome
    }
}

fn release(bytes: &[u8]) -> Release {
    Release::new("2.0.0".into(), bytes.to_vec(), sha256(bytes), 1024).unwrap()
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn write_active(root: &FixtureRoot, bytes: &[u8]) {
    fs::write(root.active(), bytes).expect("initial active artifact");
}

fn assert_file(path: impl AsRef<Path>, expected: &[u8]) {
    assert_eq!(
        fs::read(path).expect("fixture artifact remains readable"),
        expected
    );
}

#[test]
fn immutable_stage_digest_health_and_atomic_activation_leave_the_new_artifact_active() {
    let root = FixtureRoot::new();
    write_active(&root, b"verified-1.0.0");
    let store = LocalStore::new(&root);
    let health = ScriptedHealth {
        outcome: Ok(true),
        expected_version: "2.0.0",
        checked_versions: Vec::new(),
    };
    let mut coordinator = UpgradeCoordinator::new(store, LocalDigest, health);

    assert_eq!(
        coordinator
            .install("1.0.0", &release(b"verified-2.0.0"))
            .unwrap(),
        UpgradeOutcome::Activated {
            version: "2.0.0".into()
        }
    );
    let (store, _, health) = coordinator.into_parts();
    assert_eq!(store.calls, ["stage", "activate"]);
    assert_file(root.active(), b"verified-2.0.0");
    assert_file(root.0.join("previous"), b"verified-1.0.0");
    assert!(!root.0.join("stage-2.0.0").exists());
    assert_eq!(health.checked_versions, ["2.0.0"]);
}

#[test]
fn corrupt_immutable_stage_is_discarded_without_replacing_the_active_artifact() {
    let root = FixtureRoot::new();
    write_active(&root, b"verified-1.0.0");
    let mut store = LocalStore::new(&root);
    store.corrupt_staging = true;
    let health = ScriptedHealth {
        outcome: Ok(true),
        expected_version: "2.0.0",
        checked_versions: Vec::new(),
    };
    let mut coordinator = UpgradeCoordinator::new(store, LocalDigest, health);

    assert_eq!(
        coordinator.install("1.0.0", &release(b"verified-2.0.0")),
        Err(UpgradeFailure::DigestMismatch)
    );
    let (store, _, health) = coordinator.into_parts();
    assert_eq!(store.calls, ["stage", "discard"]);
    assert_file(root.active(), b"verified-1.0.0");
    assert!(!root.0.join("stage-2.0.0").exists());
    assert!(health.checked_versions.is_empty());
}

#[test]
fn unhealthy_new_version_rolls_back_once_to_the_verified_active_artifact() {
    let root = FixtureRoot::new();
    write_active(&root, b"verified-1.0.0");
    let store = LocalStore::new(&root);
    let health = ScriptedHealth {
        outcome: Ok(false),
        expected_version: "2.0.0",
        checked_versions: Vec::new(),
    };
    let mut coordinator = UpgradeCoordinator::new(store, LocalDigest, health);

    assert_eq!(
        coordinator
            .install("1.0.0", &release(b"verified-2.0.0"))
            .unwrap(),
        UpgradeOutcome::RolledBack {
            failed_version: "2.0.0".into()
        }
    );
    let (store, _, health) = coordinator.into_parts();
    assert_eq!(store.calls, ["stage", "activate", "rollback"]);
    assert_file(root.active(), b"verified-1.0.0");
    assert_file(root.0.join("failed-activation"), b"verified-2.0.0");
    assert_eq!(health.checked_versions, ["2.0.0"]);
}
