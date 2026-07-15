//! Lexical validation for untrusted root-relative path identities.
//!
//! This module deliberately does not provide filesystem confinement. A host
//! adapter must resolve a validated identity from its registered canonical
//! root, reject symlink escapes, and repeat the confinement check when opening
//! the target. Lexical validation is only the first boundary.

use std::error::Error;
use std::fmt;

/// Bounds applied while validating an untrusted path identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelativePathLimits {
    pub max_bytes: usize,
    pub max_components: usize,
    pub max_component_bytes: usize,
}

impl RelativePathLimits {
    #[must_use]
    pub const fn new(max_bytes: usize, max_components: usize, max_component_bytes: usize) -> Self {
        Self {
            max_bytes,
            max_components,
            max_component_bytes,
        }
    }
}

impl Default for RelativePathLimits {
    fn default() -> Self {
        Self::new(4_096, 128, 255)
    }
}

/// A normalized, slash-separated, non-empty root-relative identity.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RelativePath(String);

impl RelativePath {
    /// Parses an untrusted lexical path identity within explicit bounds.
    ///
    /// # Errors
    ///
    /// Returns a stable path error for invalid limits, absolute or ambiguous
    /// syntax, traversal components, controls, or exceeded resource bounds.
    pub fn parse(input: &str, limits: RelativePathLimits) -> Result<Self, RelativePathError> {
        validate_limits(limits)?;

        if input.is_empty() {
            return Err(RelativePathError::EmptyPath);
        }
        if input.len() > limits.max_bytes {
            return Err(RelativePathError::PathTooLong);
        }
        if input.starts_with('/') {
            return Err(RelativePathError::AbsolutePath);
        }
        if input.starts_with("//") || input.starts_with('\\') {
            return Err(RelativePathError::PlatformAbsolutePath);
        }

        let mut count = 0usize;
        for (index, component) in input.split('/').enumerate() {
            count = count
                .checked_add(1)
                .ok_or(RelativePathError::TooManyComponents)?;
            if count > limits.max_components {
                return Err(RelativePathError::TooManyComponents);
            }
            if component.is_empty() {
                return Err(RelativePathError::EmptyComponent { index });
            }
            if component == "." {
                return Err(RelativePathError::CurrentDirectory { index });
            }
            if component == ".." {
                return Err(RelativePathError::ParentDirectory { index });
            }
            if component.len() > limits.max_component_bytes {
                return Err(RelativePathError::ComponentTooLong { index });
            }
            if index == 0 && is_drive_prefix(component) {
                return Err(RelativePathError::PlatformAbsolutePath);
            }
            if component.contains('\\') {
                return Err(RelativePathError::PlatformSeparator { index });
            }
            if component.chars().any(char::is_control) {
                return Err(RelativePathError::ControlCharacter { index });
            }
        }

        Ok(Self(input.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn components(&self) -> impl DoubleEndedIterator<Item = &str> {
        self.0.split('/')
    }
}

impl AsRef<str> for RelativePath {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for RelativePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelativePathError {
    InvalidLimits,
    EmptyPath,
    AbsolutePath,
    PlatformAbsolutePath,
    PathTooLong,
    TooManyComponents,
    EmptyComponent { index: usize },
    CurrentDirectory { index: usize },
    ParentDirectory { index: usize },
    ComponentTooLong { index: usize },
    PlatformSeparator { index: usize },
    ControlCharacter { index: usize },
}

impl RelativePathError {
    /// Stable machine-facing error code. Display text is diagnostic only.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidLimits => "invalid_limits",
            Self::EmptyPath => "empty_path",
            Self::AbsolutePath => "absolute_path",
            Self::PlatformAbsolutePath => "platform_absolute_path",
            Self::PathTooLong => "path_too_long",
            Self::TooManyComponents => "too_many_components",
            Self::EmptyComponent { .. } => "empty_component",
            Self::CurrentDirectory { .. } => "current_directory",
            Self::ParentDirectory { .. } => "parent_directory",
            Self::ComponentTooLong { .. } => "component_too_long",
            Self::PlatformSeparator { .. } => "platform_separator",
            Self::ControlCharacter { .. } => "control_character",
        }
    }
}

impl fmt::Display for RelativePathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl Error for RelativePathError {}

fn validate_limits(limits: RelativePathLimits) -> Result<(), RelativePathError> {
    if limits.max_bytes == 0 || limits.max_components == 0 || limits.max_component_bytes == 0 {
        Err(RelativePathError::InvalidLimits)
    } else {
        Ok(())
    }
}

fn is_drive_prefix(component: &str) -> bool {
    let bytes = component.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: RelativePathLimits = RelativePathLimits::new(32, 3, 16);

    #[test]
    fn accepts_and_preserves_normalized_relative_identity() {
        let path = RelativePath::parse("docs/設計.md", LIMITS).unwrap();
        assert_eq!(path.as_str(), "docs/設計.md");
        assert_eq!(path.components().collect::<Vec<_>>(), ["docs", "設計.md"]);
    }

    #[test]
    fn rejects_empty_and_posix_absolute_paths() {
        assert_eq!(error(""), RelativePathError::EmptyPath);
        assert_eq!(error("/etc/passwd"), RelativePathError::AbsolutePath);
    }

    #[test]
    fn rejects_empty_and_dot_components_without_normalizing_them() {
        assert_eq!(
            error("a//b"),
            RelativePathError::EmptyComponent { index: 1 }
        );
        assert_eq!(error("a/"), RelativePathError::EmptyComponent { index: 1 });
        assert_eq!(
            error("./a"),
            RelativePathError::CurrentDirectory { index: 0 }
        );
        assert_eq!(
            error("a/../b"),
            RelativePathError::ParentDirectory { index: 1 }
        );
    }

    #[test]
    fn rejects_windows_separator_drive_and_unc_forms_on_every_platform() {
        assert_eq!(
            error(r"a\b"),
            RelativePathError::PlatformSeparator { index: 0 }
        );
        assert_eq!(error(r"C:\secret"), RelativePathError::PlatformAbsolutePath);
        assert_eq!(error("z:relative"), RelativePathError::PlatformAbsolutePath);
        assert_eq!(
            error(r"\\server\share"),
            RelativePathError::PlatformAbsolutePath
        );
        assert_eq!(
            error(r"\??\C:\secret"),
            RelativePathError::PlatformAbsolutePath
        );
    }

    #[test]
    fn rejects_nul_ascii_and_unicode_control_ambiguity() {
        for hostile in ["a\0b", "a\nb", "a\rb", "a\u{7f}b", "a\u{85}b"] {
            assert_eq!(
                error(hostile),
                RelativePathError::ControlCharacter { index: 0 }
            );
        }
    }

    #[test]
    fn enforces_total_component_and_depth_bounds_in_bytes() {
        assert_eq!(
            error("12345678901234567"),
            RelativePathError::ComponentTooLong { index: 0 }
        );
        assert_eq!(error("a/b/c/d"), RelativePathError::TooManyComponents);
        assert_eq!(
            error("1234567890123456/1234567890123456"),
            RelativePathError::PathTooLong
        );
        let unicode_limits = RelativePathLimits::new(8, 1, 3);
        assert_eq!(
            RelativePath::parse("🙂", unicode_limits).unwrap_err(),
            RelativePathError::ComponentTooLong { index: 0 }
        );
    }

    #[test]
    fn invalid_zero_limits_fail_closed() {
        for limits in [
            RelativePathLimits::new(0, 1, 1),
            RelativePathLimits::new(1, 0, 1),
            RelativePathLimits::new(1, 1, 0),
        ] {
            assert_eq!(
                RelativePath::parse("a", limits).unwrap_err(),
                RelativePathError::InvalidLimits
            );
        }
    }

    #[test]
    fn errors_expose_stable_codes_without_echoing_hostile_input() {
        let error = RelativePath::parse("private\nvalue", LIMITS).unwrap_err();
        assert_eq!(error.code(), "control_character");
        assert_eq!(error.to_string(), "control_character");
        assert!(!error.to_string().contains("private"));
    }

    fn error(input: &str) -> RelativePathError {
        RelativePath::parse(input, LIMITS).unwrap_err()
    }
}
