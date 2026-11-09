//! Permission keys and patterns (ADR-0008 §5).
//!
//! A key is `segment(:segment)*`, each segment `[a-z0-9_*-]` (collection names
//! may also use upper case): `projects:create`, `teams:members:manage`,
//! `org:<feature>:<permission>`, `data:<collection>:<action>`,
//! `engine:<action>`. A pattern is a key where `*` matches one segment and a
//! trailing `*` matches one or more remaining segments, so `*` is everything
//! and `teams:*` covers `teams:members:manage`. In `data:` keys the action
//! aliases `read` (list + view) and `write` (create + update + delete) apply.
//!
//! [`PermissionSet`] holds allow and deny patterns; deny always wins.

use std::collections::BTreeSet;
use std::fmt;

/// Longest key or pattern.
pub const MAX_KEY_LEN: usize = 200;

/// Validates a permission pattern (or key).
pub fn validate(pattern: &str) -> Result<(), String> {
    if pattern.is_empty() || pattern.len() > MAX_KEY_LEN {
        return Err(format!("permission {pattern:?} must be 1-{MAX_KEY_LEN} characters"));
    }
    for seg in pattern.split(':') {
        let ok =
            !seg.is_empty() && (seg == "*" || seg.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'));
        if !ok {
            return Err(format!(
                "invalid permission {pattern:?}: segments are letters, digits, _ or -, or a lone *"
            ));
        }
    }
    Ok(())
}

fn seg_matches(pattern: &str, seg: &str, last_of_data: bool) -> bool {
    if pattern == "*" || pattern.eq_ignore_ascii_case(seg) {
        return true;
    }
    last_of_data
        && match pattern {
            "read" => matches!(seg, "list" | "view"),
            "write" => matches!(seg, "create" | "update" | "delete"),
            _ => false,
        }
}

/// Whether `pattern` grants `key`.
pub fn matches(pattern: &str, key: &str) -> bool {
    let p: Vec<&str> = pattern.split(':').collect();
    let k: Vec<&str> = key.split(':').collect();
    let data = k.first() == Some(&"data") && k.len() == 3;
    for (i, ps) in p.iter().enumerate() {
        let Some(ks) = k.get(i) else { return false };
        if *ps == "*" && i == p.len() - 1 {
            return true; // trailing wildcard: the rest
        }
        if !seg_matches(ps, ks, data && i == 2) {
            return false;
        }
    }
    p.len() == k.len()
}

/// Allow and deny patterns from every source of one caller.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PermissionSet {
    /// Granted patterns.
    pub allow: BTreeSet<String>,
    /// Denied patterns (always win).
    pub deny: BTreeSet<String>,
}

/// Outcome of a permission check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grant {
    /// An explicit deny matches (wins over every grant).
    Denied,
    /// An allow matches and no deny does.
    Granted,
    /// Nothing matches.
    None,
}

impl PermissionSet {
    /// Adds every pattern of another set.
    pub fn extend(&mut self, other: &PermissionSet) {
        self.allow.extend(other.allow.iter().cloned());
        self.deny.extend(other.deny.iter().cloned());
    }

    /// Checks one key: deny first.
    pub fn check(&self, key: &str) -> Grant {
        if self.deny.iter().any(|d| matches(d, key)) {
            Grant::Denied
        } else if self.allow.iter().any(|a| matches(a, key)) {
            Grant::Granted
        } else {
            Grant::None
        }
    }

    /// Whether `key` is granted (and not denied).
    pub fn has(&self, key: &str) -> bool {
        self.check(key) == Grant::Granted
    }
}

impl fmt::Display for Grant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Grant::Denied => "denied",
            Grant::Granted => "granted",
            Grant::None => "none",
        })
    }
}

/// The key of a data-core record action on a collection.
pub fn data_key(collection: &str, action: &str) -> String {
    format!("data:{collection}:{action}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards_and_aliases() {
        assert!(matches("*", "teams:members:manage"));
        assert!(matches("teams:*", "teams:members:manage"));
        assert!(matches("teams:*", "teams:read"));
        assert!(!matches("teams:*", "teams"));
        assert!(!matches("teams:read", "teams:members:manage"));
        assert!(matches("data:*:read", "data:posts:list"));
        assert!(matches("data:*:read", "data:posts:view"));
        assert!(!matches("data:*:read", "data:posts:create"));
        assert!(matches("data:posts:write", "data:posts:delete"));
        assert!(!matches("projects:read", "projects:readx"));
        assert!(!matches("org:*:manage", "org:billing:view"));
        assert!(matches("org:*:manage", "org:billing:manage"));
        assert!(
            !matches("read", "data:x:list"),
            "aliases only in the action of data keys"
        );
    }

    #[test]
    fn deny_wins() {
        let mut s = PermissionSet::default();
        s.allow.insert("*".into());
        s.deny.insert("projects:delete".into());
        assert_eq!(s.check("projects:delete"), Grant::Denied);
        assert_eq!(s.check("projects:create"), Grant::Granted);
        assert_eq!(PermissionSet::default().check("x:y"), Grant::None);
    }

    #[test]
    fn validation() {
        assert!(validate("org:billing:manage").is_ok());
        assert!(validate("*").is_ok());
        assert!(validate("a::b").is_err());
        assert!(validate("a:b c").is_err());
        assert!(validate("a:b*").is_err());
        assert!(validate("").is_err());
    }
}
