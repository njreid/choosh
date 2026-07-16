//! Deterministic security invariant checks over structured evidence.

use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuditLimits {
    pub max_observations: usize,
    pub max_text_bytes: usize,
    pub max_findings: usize,
    pub max_accepted_risks: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Observation {
    Listener {
        id: String,
        address: String,
        transport: Transport,
    },
    Text {
        id: String,
        channel: TextChannel,
        content: String,
    },
    ResourceLimit {
        id: String,
        name: String,
        bounded: bool,
    },
    WebView {
        id: String,
        javascript_bridge: bool,
        file_access: bool,
        shared_cookie_profile: bool,
    },
    ProcessCommand {
        id: String,
        executable: String,
        arguments: Vec<String>,
        shell_interpolation: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Transport {
    UnixSocket,
    Tcp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextChannel {
    Log,
    Event,
    Configuration,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Finding {
    pub observation_id: String,
    pub code: FindingCode,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum FindingCode {
    PublicListener,
    SecretContent,
    TokenContent,
    AbsolutePathContent,
    UnboundedResource,
    WebViewJavascriptBridge,
    WebViewFileAccess,
    WebViewCookieSharing,
    ShellInterpolation,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct AcceptedRisk {
    pub risk_id: String,
    pub finding_code: FindingCode,
    pub owner: String,
    pub expires_release: String,
    pub rationale: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditReport {
    pub findings: Vec<Finding>,
    pub accepted_risks: Vec<AcceptedRisk>,
}

/// Audits bounded structured observations and preserves accepted risks separately.
///
/// Accepted risks never remove findings; consumers decide release policy while
/// retaining complete canonical evidence.
///
/// # Errors
///
/// Rejects invalid limits, excessive observations/findings/risks, duplicate or
/// invalid identities, oversized/untrusted text, and malformed accepted risks.
pub fn audit(
    observations: &[Observation],
    accepted_risks: &[AcceptedRisk],
    limits: AuditLimits,
) -> Result<AuditReport, AuditError> {
    validate_limits(limits)?;
    if observations.len() > limits.max_observations
        || accepted_risks.len() > limits.max_accepted_risks
    {
        return Err(AuditError::RecordLimit);
    }
    let mut ids = BTreeSet::new();
    let mut findings = Vec::new();
    for observation in observations {
        let id = observation_id(observation);
        validate_id(id, limits.max_text_bytes)?;
        if !ids.insert(id) {
            return Err(AuditError::DuplicateId);
        }
        inspect(observation, limits, &mut findings)?;
    }
    if findings.len() > limits.max_findings {
        return Err(AuditError::FindingLimit);
    }
    findings.sort();
    let mut risks = accepted_risks.to_vec();
    risks.sort();
    let mut risk_ids = BTreeSet::new();
    for risk in &risks {
        validate_id(&risk.risk_id, limits.max_text_bytes)?;
        validate_id(&risk.owner, limits.max_text_bytes)?;
        validate_id(&risk.expires_release, limits.max_text_bytes)?;
        validate_text(&risk.rationale, limits.max_text_bytes)?;
        if risk.rationale.is_empty() || !risk_ids.insert(risk.risk_id.as_str()) {
            return Err(AuditError::InvalidAcceptedRisk);
        }
    }
    Ok(AuditReport {
        findings,
        accepted_risks: risks,
    })
}

fn inspect(o: &Observation, limits: AuditLimits, out: &mut Vec<Finding>) -> Result<(), AuditError> {
    let id = observation_id(o);
    match o {
        Observation::Listener {
            address, transport, ..
        } => {
            validate_text(address, limits.max_text_bytes)?;
            if *transport == Transport::Tcp && !is_loopback(address) {
                add(out, id, FindingCode::PublicListener);
            }
        }
        Observation::Text { content, .. } => {
            validate_text(content, limits.max_text_bytes)?;
            let lower = content.to_ascii_lowercase();
            if ["password=", "secret=", "authorization:", "private key"]
                .iter()
                .any(|needle| lower.contains(needle))
            {
                add(out, id, FindingCode::SecretContent);
            }
            if ["token=", "bearer ", "api_key="]
                .iter()
                .any(|needle| lower.contains(needle))
            {
                add(out, id, FindingCode::TokenContent);
            }
            if contains_absolute_path(content) {
                add(out, id, FindingCode::AbsolutePathContent);
            }
        }
        Observation::ResourceLimit { name, bounded, .. } => {
            validate_id(name, limits.max_text_bytes)?;
            if !bounded {
                add(out, id, FindingCode::UnboundedResource);
            }
        }
        Observation::WebView {
            javascript_bridge,
            file_access,
            shared_cookie_profile,
            ..
        } => {
            if *javascript_bridge {
                add(out, id, FindingCode::WebViewJavascriptBridge);
            }
            if *file_access {
                add(out, id, FindingCode::WebViewFileAccess);
            }
            if *shared_cookie_profile {
                add(out, id, FindingCode::WebViewCookieSharing);
            }
        }
        Observation::ProcessCommand {
            executable,
            arguments,
            shell_interpolation,
            ..
        } => {
            validate_id(executable, limits.max_text_bytes)?;
            for argument in arguments {
                validate_text(argument, limits.max_text_bytes)?;
            }
            if *shell_interpolation
                || matches!(
                    executable.as_str(),
                    "sh" | "bash" | "zsh" | "cmd" | "powershell"
                )
            {
                add(out, id, FindingCode::ShellInterpolation);
            }
        }
    }
    Ok(())
}

fn add(out: &mut Vec<Finding>, id: &str, code: FindingCode) {
    out.push(Finding {
        observation_id: id.to_owned(),
        code,
    });
}
fn observation_id(o: &Observation) -> &str {
    match o {
        Observation::Listener { id, .. }
        | Observation::Text { id, .. }
        | Observation::ResourceLimit { id, .. }
        | Observation::WebView { id, .. }
        | Observation::ProcessCommand { id, .. } => id,
    }
}
fn is_loopback(address: &str) -> bool {
    address.starts_with("127.")
        || address.starts_with("[::1]:")
        || address.starts_with("localhost:")
}
fn contains_absolute_path(value: &str) -> bool {
    value.split_whitespace().any(|part| {
        let part = part.rsplit_once('=').map_or(part, |(_, value)| value);
        part.starts_with('/')
            || (part.len() > 2
                && part.as_bytes()[1] == b':'
                && matches!(part.as_bytes()[2], b'\\' | b'/'))
    })
}

fn validate_limits(l: AuditLimits) -> Result<(), AuditError> {
    if l.max_observations == 0
        || l.max_text_bytes == 0
        || l.max_findings == 0
        || l.max_accepted_risks == 0
    {
        Err(AuditError::InvalidLimits)
    } else {
        Ok(())
    }
}
fn validate_id(v: &str, max: usize) -> Result<(), AuditError> {
    validate_text(v, max)?;
    if v.is_empty()
        || !v
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        Err(AuditError::InvalidText)
    } else {
        Ok(())
    }
}
fn validate_text(v: &str, max: usize) -> Result<(), AuditError> {
    if v.len() > max || v.chars().any(|c| matches!(c, '\0' | '\r' | '\n')) {
        Err(AuditError::InvalidText)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuditError {
    InvalidLimits,
    RecordLimit,
    FindingLimit,
    DuplicateId,
    InvalidText,
    InvalidAcceptedRisk,
}

#[cfg(test)]
mod tests {
    use super::*;
    const L: AuditLimits = AuditLimits {
        max_observations: 16,
        max_text_bytes: 256,
        max_findings: 16,
        max_accepted_risks: 4,
    };

    #[test]
    fn detects_boundary_violations_in_canonical_order() {
        let observations = vec![
            Observation::WebView {
                id: "web".into(),
                javascript_bridge: true,
                file_access: true,
                shared_cookie_profile: true,
            },
            Observation::Listener {
                id: "listener".into(),
                address: "0.0.0.0:8080".into(),
                transport: Transport::Tcp,
            },
            Observation::ResourceLimit {
                id: "queue".into(),
                name: "events".into(),
                bounded: false,
            },
        ];
        let report = audit(&observations, &[], L).unwrap();
        assert_eq!(
            report.findings[0],
            Finding {
                observation_id: "listener".into(),
                code: FindingCode::PublicListener
            }
        );
        assert_eq!(report.findings.len(), 5);
    }

    #[test]
    fn finds_secret_token_and_absolute_paths_without_echoing_them() {
        let report = audit(
            &[Observation::Text {
                id: "log".into(),
                channel: TextChannel::Log,
                content: "token=abc file=/home/example/key".into(),
            }],
            &[],
            L,
        )
        .unwrap();
        assert_eq!(
            report
                .findings
                .iter()
                .map(|f| f.code)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([FindingCode::TokenContent, FindingCode::AbsolutePathContent])
        );
        assert!(format!("{report:?}").find("abc").is_none());
    }

    #[test]
    fn loopback_unix_and_shell_free_argv_are_clean() {
        let observations = [
            Observation::Listener {
                id: "socket".into(),
                address: "state.sock".into(),
                transport: Transport::UnixSocket,
            },
            Observation::Listener {
                id: "loop".into(),
                address: "127.0.0.1:9000".into(),
                transport: Transport::Tcp,
            },
            Observation::ProcessCommand {
                id: "git".into(),
                executable: "git".into(),
                arguments: vec!["status".into()],
                shell_interpolation: false,
            },
        ];
        assert!(audit(&observations, &[], L).unwrap().findings.is_empty());
    }

    #[test]
    fn accepted_risk_preserves_finding_and_is_sorted() {
        let observation = Observation::ResourceLimit {
            id: "queue".into(),
            name: "events".into(),
            bounded: false,
        };
        let risk = AcceptedRisk {
            risk_id: "risk-1".into(),
            finding_code: FindingCode::UnboundedResource,
            owner: "security".into(),
            expires_release: "release-2".into(),
            rationale: "Temporary compatibility investigation".into(),
        };
        let report = audit(&[observation], std::slice::from_ref(&risk), L).unwrap();
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.accepted_risks, vec![risk]);
    }

    #[test]
    fn bounds_duplicates_and_malformed_risks_fail() {
        let duplicate = [
            Observation::ResourceLimit {
                id: "same".into(),
                name: "a".into(),
                bounded: true,
            },
            Observation::ResourceLimit {
                id: "same".into(),
                name: "b".into(),
                bounded: true,
            },
        ];
        assert_eq!(audit(&duplicate, &[], L), Err(AuditError::DuplicateId));
        let risk = AcceptedRisk {
            risk_id: "r".into(),
            finding_code: FindingCode::UnboundedResource,
            owner: "security".into(),
            expires_release: "r2".into(),
            rationale: String::new(),
        };
        assert_eq!(audit(&[], &[risk], L), Err(AuditError::InvalidAcceptedRisk));
    }
}
