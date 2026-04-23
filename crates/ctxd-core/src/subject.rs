//! Subject paths for addressing context within ctxd.
//!
//! Subjects use path syntax (`/work/acme/customers/cust-42`), not dotted notation.
//! Read operations support a recursive flag and glob wildcards (`*`, `**`).

use serde::{Deserialize, Serialize};
use std::fmt;

/// A validated subject path.
///
/// Subject paths must:
/// - Start with `/`
/// - Contain only alphanumeric characters, hyphens, underscores, and `/` separators
/// - Not contain empty segments (`//`)
/// - Not end with `/` (except the root `/`)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Subject(String);

/// Errors that can occur when parsing a subject path.
#[derive(Debug, thiserror::Error)]
pub enum SubjectError {
    /// The path is empty.
    #[error("subject path must not be empty")]
    Empty,

    /// The path doesn't start with `/`.
    #[error("subject path must start with '/'")]
    MissingLeadingSlash,

    /// The path contains an empty segment (`//`).
    #[error("subject path must not contain empty segments")]
    EmptySegment,

    /// The path contains invalid characters.
    #[error("subject path contains invalid character: {0}")]
    InvalidCharacter(char),
}

impl Subject {
    /// Parse and validate a subject path.
    pub fn new(path: &str) -> Result<Self, SubjectError> {
        if path.is_empty() {
            return Err(SubjectError::Empty);
        }
        if !path.starts_with('/') {
            return Err(SubjectError::MissingLeadingSlash);
        }
        // Root path is valid
        if path == "/" {
            return Ok(Self(path.to_string()));
        }
        // Check for trailing slash
        if path.ends_with('/') {
            return Err(SubjectError::EmptySegment);
        }
        // Check for empty segments
        if path.contains("//") {
            return Err(SubjectError::EmptySegment);
        }
        // Validate characters in segments
        for ch in path.chars().skip(1) {
            if ch != '/' && ch != '-' && ch != '_' && ch != '.' && !ch.is_alphanumeric() {
                return Err(SubjectError::InvalidCharacter(ch));
            }
        }
        Ok(Self(path.to_string()))
    }

    /// Returns the path string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Check if `other` is a descendant of this subject (for recursive reads).
    ///
    /// `/test` is a parent of `/test/hello` and `/test/hello/world`.
    /// `/test` is NOT a parent of `/testing`.
    /// The root `/` is a parent of everything.
    pub fn is_parent_of(&self, other: &Subject) -> bool {
        if self.0 == "/" {
            return true;
        }
        other.0.starts_with(&self.0)
            && other.0.len() > self.0.len()
            && other.0.as_bytes()[self.0.len()] == b'/'
    }

    /// Check if this subject matches a glob pattern.
    ///
    /// Supports `*` (single segment) and `**` (any number of segments).
    /// Pattern must start with `/`.
    pub fn matches_glob(&self, pattern: &str) -> bool {
        glob_match::glob_match(pattern, &self.0)
    }

    /// Check if a subject path matches a capability-style pattern.
    ///
    /// Supports `/**` (any depth) and `/*` (single segment) suffixes.
    /// Used by the capability engine and wire protocol for subject scoping.
    pub fn matches_cap_pattern(subject: &str, pattern: &str) -> bool {
        if pattern == "/**" {
            return true;
        }
        if pattern == subject {
            return true;
        }
        if let Some(prefix) = pattern.strip_suffix("/**") {
            return subject == prefix || subject.starts_with(&format!("{prefix}/"));
        }
        if let Some(prefix) = pattern.strip_suffix("/*") {
            if !subject.starts_with(&format!("{prefix}/")) {
                return false;
            }
            let rest = &subject[prefix.len() + 1..];
            return !rest.contains('/');
        }
        false
    }

    /// Returns true if `self` exactly equals `other`, or (when `recursive` is true)
    /// if `other` is a descendant of `self`.
    pub fn matches(&self, other: &Subject, recursive: bool) -> bool {
        if self == other {
            return true;
        }
        if recursive {
            return self.is_parent_of(other);
        }
        false
    }
}

impl fmt::Display for Subject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<Subject> for String {
    fn from(s: Subject) -> String {
        s.0
    }
}

impl TryFrom<String> for Subject {
    type Error = SubjectError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Subject::new(&s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_paths() {
        assert!(Subject::new("/").is_ok());
        assert!(Subject::new("/test").is_ok());
        assert!(Subject::new("/test/hello").is_ok());
        assert!(Subject::new("/work/acme/customers/cust-42").is_ok());
        assert!(Subject::new("/a-b_c.d").is_ok());
    }

    #[test]
    fn invalid_paths() {
        assert!(Subject::new("").is_err());
        assert!(Subject::new("no-slash").is_err());
        assert!(Subject::new("/trailing/").is_err());
        assert!(Subject::new("/empty//segment").is_err());
        assert!(Subject::new("/bad char").is_err());
    }

    #[test]
    fn parent_child_relationship() {
        let parent = Subject::new("/test").unwrap();
        let child = Subject::new("/test/hello").unwrap();
        let grandchild = Subject::new("/test/hello/world").unwrap();
        let unrelated = Subject::new("/testing").unwrap();

        assert!(parent.is_parent_of(&child));
        assert!(parent.is_parent_of(&grandchild));
        assert!(!parent.is_parent_of(&unrelated));
        assert!(!parent.is_parent_of(&parent));
    }

    #[test]
    fn root_is_parent_of_everything() {
        let root = Subject::new("/").unwrap();
        let child = Subject::new("/anything").unwrap();
        let deep = Subject::new("/a/b/c/d").unwrap();

        assert!(root.is_parent_of(&child));
        assert!(root.is_parent_of(&deep));
    }

    #[test]
    fn exact_match() {
        let a = Subject::new("/test/hello").unwrap();
        let b = Subject::new("/test/hello").unwrap();
        let c = Subject::new("/test/other").unwrap();

        assert!(a.matches(&b, false));
        assert!(!a.matches(&c, false));
    }

    #[test]
    fn recursive_match() {
        let parent = Subject::new("/test").unwrap();
        let child = Subject::new("/test/hello").unwrap();
        let unrelated = Subject::new("/other").unwrap();

        assert!(parent.matches(&child, true));
        assert!(!parent.matches(&unrelated, true));
        // Exact match should work with recursive=true too
        assert!(parent.matches(&parent, true));
    }

    #[test]
    fn glob_matching() {
        let subject = Subject::new("/work/acme/customers/cust-42").unwrap();

        assert!(subject.matches_glob("/work/acme/customers/*"));
        assert!(subject.matches_glob("/work/acme/**"));
        assert!(subject.matches_glob("/work/**"));
        assert!(subject.matches_glob("/**"));
        assert!(!subject.matches_glob("/work/other/*"));
    }

    #[test]
    fn serialization_roundtrip() {
        let subject = Subject::new("/test/hello").unwrap();
        let json = serde_json::to_string(&subject).unwrap();
        assert_eq!(json, "\"/test/hello\"");

        let deserialized: Subject = serde_json::from_str(&json).unwrap();
        assert_eq!(subject, deserialized);
    }
}
