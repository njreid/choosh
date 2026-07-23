//! Shell-free per-user daemon service-manager adapters.
//!
//! Deployment orchestration injects one adapter and a direct process runner.
//! This module owns neither release files nor SSH credentials. Unsupported
//! platforms fail closed rather than emulating daemon persistence with a shell.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

const SYSTEMD_UNIT: &str = "chooshd.service";
const LAUNCHD_LABEL: &str = "ai.choosh.chooshd";

/// Exact, shell-free manager program and argument vector.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceInvocation {
    program: &'static str,
    arguments: Vec<OsString>,
}

impl ServiceInvocation {
    fn new(program: &'static str, arguments: impl IntoIterator<Item = OsString>) -> Self {
        Self {
            program,
            arguments: arguments.into_iter().collect(),
        }
    }

    /// Returns the fixed executable name for a direct-process outer adapter.
    #[must_use]
    pub const fn program(&self) -> &'static str {
        self.program
    }

    /// Returns the already separated argv values; no shell text is represented.
    #[must_use]
    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }
}

/// Result reported by a direct manager-process runner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessOutcome {
    Success,
    Rejected,
}

/// Direct process capability implemented at the deployment composition root.
pub trait ServiceProcessRunner {
    type Error;

    /// Executes exactly one manager argv vector without a shell.
    ///
    /// # Errors
    ///
    /// Returns the injected runner's failure when the direct process cannot run.
    fn run(&mut self, invocation: ServiceInvocation) -> Result<ProcessOutcome, Self::Error>;
}

/// Starts or stops the per-user `chooshd` service after a release is selected.
pub trait ServiceManager {
    type Error;

    /// Activates the fixed daemon unit.
    ///
    /// # Errors
    ///
    /// Returns a typed runner, rejection, or unsupported-manager failure.
    fn activate(&mut self) -> Result<(), Self::Error>;

    /// Stops the fixed daemon unit without discovering or killing arbitrary processes.
    ///
    /// # Errors
    ///
    /// Returns a typed runner, rejection, or unsupported-manager failure.
    fn stop(&mut self) -> Result<(), Self::Error>;
}

/// `systemd --user` adapter for the fixed daemon unit.
pub struct SystemdUserManager<R> {
    runner: R,
}

impl<R> SystemdUserManager<R> {
    /// Creates an adapter with an injected direct process runner.
    #[must_use]
    pub const fn new(runner: R) -> Self {
        Self { runner }
    }

    /// Returns the injected runner after a test or outer composition completes.
    #[must_use]
    pub fn into_inner(self) -> R {
        self.runner
    }
}

impl<R: ServiceProcessRunner> SystemdUserManager<R> {
    fn invoke(&mut self, arguments: &[&str]) -> Result<(), ServiceManagerError<R::Error>> {
        let invocation = ServiceInvocation::new(
            "systemctl",
            arguments.iter().map(|argument| OsString::from(*argument)),
        );
        match self
            .runner
            .run(invocation)
            .map_err(ServiceManagerError::Runner)?
        {
            ProcessOutcome::Success => Ok(()),
            ProcessOutcome::Rejected => Err(ServiceManagerError::Rejected),
        }
    }
}

impl<R: ServiceProcessRunner> ServiceManager for SystemdUserManager<R> {
    type Error = ServiceManagerError<R::Error>;

    fn activate(&mut self) -> Result<(), Self::Error> {
        self.invoke(&["--user", "daemon-reload"])?;
        self.invoke(&["--user", "enable", "--now", SYSTEMD_UNIT])
    }

    fn stop(&mut self) -> Result<(), Self::Error> {
        self.invoke(&["--user", "disable", "--now", SYSTEMD_UNIT])
    }
}

/// Validated launchd GUI user identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LaunchdUser(u32);

impl LaunchdUser {
    /// Rejects zero, which is not a supported per-user GUI domain.
    #[must_use]
    pub const fn new(value: u32) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    fn domain(self) -> OsString {
        OsString::from(format!("gui/{}", self.0))
    }
}

/// Absolute normalized launchd plist path held only at the deployment boundary.
#[derive(Clone, Eq, PartialEq)]
pub struct LaunchdPlist(PathBuf);

impl LaunchdPlist {
    /// Validates a normalized absolute plist path without resolving symlinks or filesystem state.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceManagerError::InvalidLaunchdPlist`] for a relative, non-UTF-8, or
    /// lexically non-normalized path.
    pub fn new(
        path: impl Into<PathBuf>,
    ) -> Result<Self, ServiceManagerError<std::convert::Infallible>> {
        let path = path.into();
        if !is_absolute_normalized(&path) {
            return Err(ServiceManagerError::InvalidLaunchdPlist);
        }
        Ok(Self(path))
    }
}

impl std::fmt::Debug for LaunchdPlist {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("LaunchdPlist(REDACTED)")
    }
}

/// `launchd` adapter for the fixed daemon label.
pub struct LaunchdManager<R> {
    runner: R,
    user: LaunchdUser,
    plist: LaunchdPlist,
}

impl<R> LaunchdManager<R> {
    /// Creates an adapter only from validated per-user launchd inputs.
    #[must_use]
    pub const fn new(runner: R, user: LaunchdUser, plist: LaunchdPlist) -> Self {
        Self {
            runner,
            user,
            plist,
        }
    }

    /// Returns the injected runner after a test or outer composition completes.
    #[must_use]
    pub fn into_inner(self) -> R {
        self.runner
    }
}

impl<R: ServiceProcessRunner> LaunchdManager<R> {
    fn invoke(&mut self, arguments: Vec<OsString>) -> Result<(), ServiceManagerError<R::Error>> {
        match self
            .runner
            .run(ServiceInvocation::new("launchctl", arguments))
            .map_err(ServiceManagerError::Runner)?
        {
            ProcessOutcome::Success => Ok(()),
            ProcessOutcome::Rejected => Err(ServiceManagerError::Rejected),
        }
    }
}

impl<R: ServiceProcessRunner> ServiceManager for LaunchdManager<R> {
    type Error = ServiceManagerError<R::Error>;

    fn activate(&mut self) -> Result<(), Self::Error> {
        let domain = self.user.domain();
        self.invoke(vec![
            OsString::from("bootstrap"),
            domain.clone(),
            self.plist.0.as_os_str().to_os_string(),
        ])?;
        self.invoke(vec![
            OsString::from("kickstart"),
            OsString::from("-k"),
            OsString::from(format!("{}/{}", domain.to_string_lossy(), LAUNCHD_LABEL)),
        ])
    }

    fn stop(&mut self) -> Result<(), Self::Error> {
        self.invoke(vec![
            OsString::from("bootout"),
            OsString::from(format!(
                "{}/{}",
                self.user.domain().to_string_lossy(),
                LAUNCHD_LABEL
            )),
        ])
    }
}

/// Stable failure classification; inner runner errors remain opaque to this module.
pub enum ServiceManagerError<E> {
    Unsupported,
    InvalidLaunchdPlist,
    Rejected,
    Runner(E),
}

impl<E> std::fmt::Debug for ServiceManagerError<E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => formatter.write_str("ServiceManagerError::Unsupported"),
            Self::InvalidLaunchdPlist => {
                formatter.write_str("ServiceManagerError::InvalidLaunchdPlist")
            }
            Self::Rejected => formatter.write_str("ServiceManagerError::Rejected"),
            Self::Runner(_) => formatter.write_str("ServiceManagerError::Runner(REDACTED)"),
        }
    }
}

/// Explicit unsupported-host manager. It cannot spawn or background a process.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnsupportedServiceManager;

impl ServiceManager for UnsupportedServiceManager {
    type Error = ServiceManagerError<std::convert::Infallible>;

    fn activate(&mut self) -> Result<(), Self::Error> {
        Err(ServiceManagerError::Unsupported)
    }

    fn stop(&mut self) -> Result<(), Self::Error> {
        Err(ServiceManagerError::Unsupported)
    }
}

fn is_absolute_normalized(path: &Path) -> bool {
    let Some(text) = path.to_str() else {
        return false;
    };
    path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
        && text.strip_prefix('/').is_some_and(|suffix| {
            suffix
                .split('/')
                .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
        })
}

#[cfg(test)]
mod tests {
    use super::{
        LaunchdManager, LaunchdPlist, LaunchdUser, ProcessOutcome, ServiceInvocation,
        ServiceManager, ServiceManagerError, ServiceProcessRunner, SystemdUserManager,
        UnsupportedServiceManager,
    };
    use std::ffi::OsString;

    #[derive(Default)]
    struct RecordingRunner {
        calls: Vec<ServiceInvocation>,
        reject_at: Option<usize>,
    }

    impl ServiceProcessRunner for RecordingRunner {
        type Error = ();

        fn run(&mut self, invocation: ServiceInvocation) -> Result<ProcessOutcome, Self::Error> {
            self.calls.push(invocation);
            Ok(if self.reject_at == Some(self.calls.len()) {
                ProcessOutcome::Rejected
            } else {
                ProcessOutcome::Success
            })
        }
    }

    fn argv(call: &ServiceInvocation) -> Vec<&str> {
        call.arguments()
            .iter()
            .map(|argument| argument.to_str().unwrap())
            .collect()
    }

    #[test]
    fn systemd_user_activation_and_stop_are_fixed_shell_free_vectors() {
        let mut manager = SystemdUserManager::new(RecordingRunner::default());
        manager.activate().unwrap();
        manager.stop().unwrap();
        let runner = manager.into_inner();
        assert_eq!(runner.calls.len(), 3);
        assert_eq!(runner.calls[0].program(), "systemctl");
        assert_eq!(argv(&runner.calls[0]), ["--user", "daemon-reload"]);
        assert_eq!(
            argv(&runner.calls[1]),
            ["--user", "enable", "--now", "chooshd.service"]
        );
        assert_eq!(
            argv(&runner.calls[2]),
            ["--user", "disable", "--now", "chooshd.service"]
        );
    }

    #[test]
    fn manager_rejection_stops_activation_before_later_commands() {
        let mut manager = SystemdUserManager::new(RecordingRunner {
            calls: Vec::new(),
            reject_at: Some(1),
        });
        assert!(matches!(
            manager.activate(),
            Err(ServiceManagerError::Rejected)
        ));
        assert_eq!(manager.into_inner().calls.len(), 1);
    }

    #[test]
    fn launchd_vectors_are_bounded_to_validated_domain_and_fixed_label() {
        let plist = LaunchdPlist::new("/opt/choosh/current/chooshd.plist").unwrap();
        let mut manager = LaunchdManager::new(
            RecordingRunner::default(),
            LaunchdUser::new(501).unwrap(),
            plist,
        );
        manager.activate().unwrap();
        manager.stop().unwrap();
        let runner = manager.into_inner();
        assert_eq!(runner.calls.len(), 3);
        assert_eq!(runner.calls[0].program(), "launchctl");
        assert_eq!(
            argv(&runner.calls[0]),
            ["bootstrap", "gui/501", "/opt/choosh/current/chooshd.plist"]
        );
        assert_eq!(
            argv(&runner.calls[1]),
            ["kickstart", "-k", "gui/501/ai.choosh.chooshd"]
        );
        assert_eq!(
            argv(&runner.calls[2]),
            ["bootout", "gui/501/ai.choosh.chooshd"]
        );
    }

    #[test]
    fn invalid_or_unsupported_hosts_fail_closed_without_invocation() {
        assert!(LaunchdUser::new(0).is_none());
        for path in [
            "relative.plist",
            "/opt/choosh/../evil.plist",
            "/opt/choosh/./daemon.plist",
        ] {
            assert!(matches!(
                LaunchdPlist::new(path),
                Err(ServiceManagerError::InvalidLaunchdPlist)
            ));
        }
        let mut unsupported = UnsupportedServiceManager;
        assert!(matches!(
            unsupported.activate(),
            Err(ServiceManagerError::Unsupported)
        ));
        assert!(matches!(
            unsupported.stop(),
            Err(ServiceManagerError::Unsupported)
        ));
        assert_eq!(
            format!("{:?}", LaunchdPlist::new("/secret/daemon.plist").unwrap()),
            "LaunchdPlist(REDACTED)"
        );
        assert_eq!(OsString::from("systemctl"), OsString::from("systemctl"));
    }
}
