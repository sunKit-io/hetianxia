use crate::error::{Error, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use std::fmt;
use unicode_normalization::UnicodeNormalization;

/// NFC、用 `/` 分隔、拒绝 `..` 和绝对路径。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RelPath(String);

impl RelPath {
    pub fn parse(raw: &str) -> Result<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(Error::new(ErrorCode::InvalidLocation, "空路径"));
        }
        if raw.starts_with('/') || raw.starts_with('~') || raw.contains('\\') || raw.contains('\0') || raw.contains(':') {
            return Err(Error::new(ErrorCode::ZipSlip, format!("非法路径 {raw}")));
        }
        let raw = raw.trim_matches('/');
        if raw.is_empty() {
            return Err(Error::new(ErrorCode::InvalidLocation, "空路径"));
        }
        let nfc: String = raw.nfc().collect();
        for part in nfc.split('/') {
            if part.is_empty() || part == "." || part == ".." {
                return Err(Error::new(ErrorCode::ZipSlip, format!("路径穿越 {raw}")));
            }
        }
        Ok(RelPath(nfc))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }

    pub fn parent(&self) -> Option<RelPath> {
        self.0.rfind('/').map(|i| RelPath(self.0[..i].to_string()))
    }

    pub fn join(&self, child: &str) -> Result<RelPath> {
        RelPath::parse(&format!("{}/{}", self.0, child))
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone)]
pub struct Ignore {
    pub patterns: Vec<String>,
}

impl Ignore {
    pub fn defaults() -> Self {
        Self {
            patterns: [".git", "node_modules", ".DS_Store", "Thumbs.db", "*.tmp", ".idea"]
                .into_iter()
                .map(|s| s.trim().to_string())
                .collect(),
        }
    }

    pub fn matches(&self, rel: &str) -> bool {
        let rel = rel.trim_matches('/');
        self.patterns.iter().any(|p| glob_match(p, rel))
    }
}

fn glob_match(pattern: &str, path: &str) -> bool {
    if pattern.contains('/') {
        return wild(pattern, path);
    }
    path.split('/').any(|seg| wild(pattern, seg)) || wild(pattern, path)
}

fn wild(pat: &str, text: &str) -> bool {
    fn rec(p: &[u8], t: &[u8]) -> bool {
        if p.is_empty() {
            return t.is_empty();
        }
        if p[0] == b'*' {
            rec(&p[1..], t) || (!t.is_empty() && rec(p, &t[1..]))
        } else if !t.is_empty() && (p[0] == b'?' || p[0] == t[0]) {
            rec(&p[1..], &t[1..])
        } else {
            false
        }
    }
    rec(pat.as_bytes(), text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal() {
        assert!(RelPath::parse("../etc/passwd").is_err());
        assert!(RelPath::parse("/tmp/x").is_err());
        assert!(RelPath::parse("a/../../b").is_err());
        assert!(RelPath::parse(r"a\..\x").is_err());
        assert_eq!(RelPath::parse("a/b").unwrap().as_str(), "a/b");
    }

    #[test]
    fn ignore_defaults() {
        let ig = Ignore::defaults();
        assert!(ig.matches(".git/config"));
        assert!(ig.matches("src/node_modules/x"));
        assert!(ig.matches("foo/.DS_Store"));
        assert!(ig.matches("a.tmp"));
        assert!(!ig.matches("src/app.js"));
        assert_eq!(ig.patterns.len(), 6);
    }
}
