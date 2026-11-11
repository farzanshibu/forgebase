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
//!
//! **Resource keys** (FB-134): the engine actions of [`RESOURCE_ACTIONS`]
//! take a resource segment, `engine:<action>:<name>` (`engine:kv_read:cache`,
//! `engine:queue_consume:orders:billing`). The name is everything after the
//! second `:` (it may contain `:` and `.`), compared case-sensitively; in a
//! pattern it may hold `*` wildcards that match any characters
//! (`engine:kv_read:cache.*`). The plain key `engine:<action>` keeps meaning
//! "every resource": a grant of it grants every resource key of that action,
//! and a deny of it denies them all.

use std::collections::BTreeSet;
use std::fmt;

/// Longest key or pattern.
pub const MAX_KEY_LEN: usize = 200;

/// Engine actions whose keys take a resource segment
/// (`engine:<action>:<name>`). Other engine keys with three segments (such
/// as `engine:read_jobs:all`) are ordinary keys: `engine:<action>` does not
/// grant them.
pub const RESOURCE_ACTIONS: [&str; 17] = [
    "kv_read",
    "kv_write",
    "kv_admin",
    "vector_read",
    "vector_write",
    "vector_admin",
    "graph_read",
    "graph_write",
    "storage_read",
    "storage_write",
    "storage_admin",
    "queue_read",
    "queue_publish",
    "queue_consume",
    "queue_admin",
    "channel_subscribe",
    "channel_publish",
];

/// `(action, name)` of a resource key or pattern `engine:<action>:<name>`
/// (the action may be `*` in a pattern).
fn resource_parts(s: &str) -> Option<(&str, &str)> {
    let rest = s.strip_prefix("engine:")?;
    let (action, name) = rest.split_once(':')?;
    (action == "*" || RESOURCE_ACTIONS.contains(&action)).then_some((action, name))
}

/// The resource key of `action` (snake case, as in keys) on `name`.
pub fn resource_key(action: &str, name: &str) -> String {
    format!("engine:{action}:{name}")
}

/// `engine:<action>` of a resource key, which grants every resource.
pub fn resource_base(key: &str) -> Option<&str> {
    let (action, _) = resource_parts(key)?;
    (action != "*").then(|| &key[.."engine:".len() + action.len()])
}

/// Glob match: `*` in `pattern` matches any run of characters.
fn glob(pattern: &str, s: &str) -> bool {
    let p = pattern.as_bytes();
    let t = s.as_bytes();
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && p[pi] != b'*' && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            pi += 1;
            mark = ti;
        } else if let Some(sp) = star {
            pi = sp + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == b'*')
}

/// Validates a permission pattern (or key).
pub fn validate(pattern: &str) -> Result<(), String> {
    if pattern.is_empty() || pattern.len() > MAX_KEY_LEN {
        return Err(format!("permission {pattern:?} must be 1-{MAX_KEY_LEN} characters"));
    }
    if let Some((_, name)) = resource_parts(pattern) {
        let ok = !name.is_empty() && !name.chars().any(|c| c.is_control() || c.is_whitespace());
        return if ok {
            Ok(())
        } else {
            Err(format!(
                "invalid permission {pattern:?}: the resource name after engine:<action>: must be non-empty, without spaces"
            ))
        };
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

/// Whether `pattern` matches `key` exactly as written (no base-key rule:
/// see [`grants`]).
pub fn matches(pattern: &str, key: &str) -> bool {
    if let Some((ka, kn)) = resource_parts(key).filter(|(a, _)| *a != "*") {
        // Resource keys: the name is one segment, whatever it contains.
        let p: Vec<&str> = pattern.splitn(3, ':').collect();
        return match p.as_slice() {
            ["*"] => true,
            [e, "*"] => seg_matches(e, "engine", false),
            [e, a, n] => seg_matches(e, "engine", false) && seg_matches(a, ka, false) && glob(n, kn),
            _ => false,
        };
    }
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

    /// Checks one key: deny first. A resource key is also granted (or
    /// denied) through its base key `engine:<action>`.
    pub fn check(&self, key: &str) -> Grant {
        if self.deny.iter().any(|d| grants(d, key)) {
            Grant::Denied
        } else if self.allow.iter().any(|a| grants(a, key)) {
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

/// Whether `pattern` covers `key`: [`matches`], or for a resource key a
/// match of its base key `engine:<action>` ("every resource").
pub fn grants(pattern: &str, key: &str) -> bool {
    matches(pattern, key) || resource_base(key).is_some_and(|b| matches(pattern, b))
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
    fn resource_keys() {
        let k = "engine:kv_read:cache.users";
        assert!(!matches("engine:kv_read", k), "exact match only");
        assert!(grants("engine:kv_read", k), "the base key grants every resource");
        assert!(grants("engine:*", k));
        assert!(grants("*", k));
        assert!(grants("engine:kv_read:*", k));
        assert!(grants("engine:kv_read:cache.*", k));
        assert!(grants("engine:*:cache.users", k));
        assert!(!grants("engine:kv_read:cache", k));
        assert!(!grants("engine:kv_read:Cache.users", k), "names are case-sensitive");
        assert!(!grants("engine:kv_write", k));
        assert!(
            !grants("engine:kv_read:cache.users", "engine:kv_read"),
            "a resource grant is not global"
        );
        assert!(grants(
            "engine:queue_consume:orders:*",
            "engine:queue_consume:orders:billing"
        ));
        assert!(!grants(
            "engine:queue_consume:orders",
            "engine:queue_consume:orders:billing"
        ));
        assert!(
            !grants("engine:read_jobs", "engine:read_jobs:all"),
            "not a resource action"
        );
        assert!(glob("a*b*c", "aXbYc") && glob("*", "") && glob("a*", "a") && !glob("a*b", "abc"));
        assert_eq!(resource_base(k), Some("engine:kv_read"));
        assert_eq!(resource_base("engine:read_jobs:all"), None);

        let mut s = PermissionSet::default();
        s.allow.insert("engine:kv_read".into());
        s.deny.insert("engine:kv_read:secret*".into());
        assert_eq!(s.check("engine:kv_read:cache"), Grant::Granted);
        assert_eq!(s.check("engine:kv_read:secrets"), Grant::Denied);
        assert_eq!(s.check("engine:kv_read"), Grant::Granted);
        let mut d = PermissionSet::default();
        d.allow.insert("engine:kv_read:*".into());
        d.deny.insert("engine:kv_read".into());
        assert_eq!(
            d.check("engine:kv_read:cache"),
            Grant::Denied,
            "denying the base denies every resource"
        );
    }

    #[test]
    fn validation() {
        assert!(validate("engine:kv_read:cache.users").is_ok());
        assert!(validate("engine:kv_read:cache.*").is_ok());
        assert!(validate("engine:queue_consume:orders:billing").is_ok());
        assert!(validate("engine:*:x").is_ok());
        assert!(validate("engine:kv_read:").is_err());
        assert!(validate("engine:kv_read:a b").is_err());
        assert!(validate("engine:read_jobs:a.b").is_err(), "not a resource action");
        assert!(validate("org:billing:manage").is_ok());
        assert!(validate("*").is_ok());
        assert!(validate("a::b").is_err());
        assert!(validate("a:b c").is_err());
        assert!(validate("a:b*").is_err());
        assert!(validate("").is_err());
    }
}
