//! Path parsing and normalisation for VFS operations.

use std::fmt;

/// A normalised, slash-separated path. No leading `/`, no `.`/`..` components.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct VfsPath {
    segments: Vec<String>,
}

impl VfsPath {
    pub fn root() -> Self {
        Self { segments: vec![] }
    }

    pub fn parse(input: &str) -> Result<Self, PathError> {
        let mut out = Vec::new();
        for raw in input.split('/') {
            if raw.is_empty() || raw == "." {
                continue;
            }
            if raw == ".." {
                if out.pop().is_none() {
                    return Err(PathError::Escape);
                }
                continue;
            }
            if raw.contains('\\') || raw.contains('\0') {
                return Err(PathError::InvalidSegment(raw.to_string()));
            }
            out.push(raw.to_string());
        }
        Ok(Self { segments: out })
    }

    pub fn is_root(&self) -> bool {
        self.segments.is_empty()
    }

    pub fn segments(&self) -> &[String] {
        &self.segments
    }

    pub fn first(&self) -> Option<&str> {
        self.segments.first().map(|s| s.as_str())
    }

    /// Drop the leading segment and return the remainder.
    pub fn shift(&self) -> VfsPath {
        if self.segments.is_empty() {
            self.clone()
        } else {
            VfsPath {
                segments: self.segments[1..].to_vec(),
            }
        }
    }

    /// Return a new path with `seg` prepended.
    pub fn prepend(&self, seg: &str) -> VfsPath {
        let mut s = vec![seg.to_string()];
        s.extend(self.segments.iter().cloned());
        VfsPath { segments: s }
    }

    pub fn join(&self, seg: &str) -> VfsPath {
        let mut s = self.segments.clone();
        s.push(seg.to_string());
        VfsPath { segments: s }
    }

    pub fn to_string_path(&self) -> String {
        if self.segments.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", self.segments.join("/"))
        }
    }
}

impl fmt::Display for VfsPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_string_path())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PathError {
    #[error("path escapes root")]
    Escape,
    #[error("invalid segment: {0}")]
    InvalidSegment(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple() {
        let p = VfsPath::parse("/chains/ethereum/head/number").unwrap();
        assert_eq!(p.segments(), &["chains", "ethereum", "head", "number"]);
    }

    #[test]
    fn root_normalises() {
        assert!(VfsPath::parse("").unwrap().is_root());
        assert!(VfsPath::parse("/").unwrap().is_root());
        assert!(VfsPath::parse("//").unwrap().is_root());
    }

    #[test]
    fn rejects_escape() {
        assert!(VfsPath::parse("../etc").is_err());
    }

    #[test]
    fn dotdot_in_middle_pops() {
        let p = VfsPath::parse("a/b/../c").unwrap();
        assert_eq!(p.segments(), &["a", "c"]);
    }

    #[test]
    fn shift_drops_first() {
        let p = VfsPath::parse("a/b/c").unwrap();
        assert_eq!(p.shift().segments(), &["b", "c"]);
    }

    #[test]
    fn display_round_trip() {
        let p = VfsPath::parse("/x/y").unwrap();
        assert_eq!(p.to_string_path(), "/x/y");
        assert_eq!(VfsPath::root().to_string_path(), "/");
    }
}
