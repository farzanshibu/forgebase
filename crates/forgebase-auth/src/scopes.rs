//! Scopes of API keys and personal access tokens.
//!
//! Grammar: `*` | `<area>:<access>` | `<area>:<access>:<resource>` |
//! `<area>:*`, with `access` = `read` | `write`. `write` does not imply
//! `read`: grant both when both are needed.
//!
//! A scope only *narrows* what the credential's owner may do; authorization
//! (FB-21 policies, the data core's fail-closed default) still applies to the
//! owner's principal.
//!
//! Enforcement is central, in the auth layer, from the request alone, so it
//! holds for every route including ones added later:
//!
//! | Request path | Area | Resource |
//! |---|---|---|
//! | `/api/auth/...` | `auth` | — |
//! | `/api/system/...`, `/metrics` | `system` | — |
//! | `/api/v1/<seg>/<name>/...` | `<seg>` (e.g. `collections`, `sql`) | `<name>` |
//!
//! `GET`/`HEAD`/`OPTIONS` need `read`, every other method `write`. Paths
//! outside `/api/` (health, dashboard assets) need no scope, except
//! `/metrics`, which is operational data like `/api/system/*` (FB-80).

use std::fmt;

use axum::http::Method;
use serde::{Deserialize, Serialize};

/// Read or write access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Safe methods.
    Read,
    /// Everything else.
    Write,
}

impl Access {
    fn as_str(self) -> &'static str {
        match self {
            Access::Read => "read",
            Access::Write => "write",
        }
    }
}

/// The scope a request needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredScope {
    /// Area, e.g. `collections`.
    pub area: String,
    /// Access.
    pub access: Access,
    /// Resource (collection name), if the path names one.
    pub resource: Option<String>,
}

impl fmt::Display for RequiredScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.area, self.access.as_str())?;
        if let Some(r) = &self.resource {
            write!(f, ":{r}")?;
        }
        Ok(())
    }
}

/// The scope a request needs, or `None` for paths outside the API.
pub fn required_scope(method: &Method, path: &str) -> Option<RequiredScope> {
    let access = if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        Access::Read
    } else {
        Access::Write
    };
    if path == "/metrics" {
        return Some(RequiredScope {
            area: "system".to_owned(),
            access,
            resource: None,
        });
    }
    let rest = path.strip_prefix("/api/")?;
    let mut segs = rest.split('/').filter(|s| !s.is_empty());
    let first = segs.next()?;
    let (area, resource) = match first {
        "auth" | "system" => (first.to_owned(), None),
        "v1" | "v2" => {
            let area = segs.next().unwrap_or("api").to_owned();
            (area, segs.next().map(str::to_owned))
        }
        other => (other.to_owned(), None),
    };
    Some(RequiredScope { area, access, resource })
}

/// A validated scope string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Scope(String);

impl Scope {
    /// Parses and validates a scope.
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s == "*" {
            return Ok(Self(s.to_owned()));
        }
        let parts: Vec<&str> = s.split(':').collect();
        let ident = |p: &str, extra: bool| {
            !p.is_empty()
                && p.len() <= 64
                && p.bytes().enumerate().all(|(i, b)| {
                    b.is_ascii_lowercase()
                        || (i > 0 && (b.is_ascii_digit() || b == b'_' || b == b'-'))
                        || (extra && (b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
                })
        };
        let ok = match parts.as_slice() {
            [area, "*"] => ident(area, false),
            [area, access] => ident(area, false) && matches!(*access, "read" | "write"),
            [area, access, res] => ident(area, false) && matches!(*access, "read" | "write") && ident(res, true),
            _ => false,
        };
        if ok {
            Ok(Self(s.to_owned()))
        } else {
            Err(format!(
                "invalid scope {s:?}: expected '*', '<area>:*', '<area>:read|write' or '<area>:read|write:<name>'"
            ))
        }
    }

    /// The scope text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this scope grants `req`.
    pub fn grants(&self, req: &RequiredScope) -> bool {
        if self.0 == "*" {
            return true;
        }
        let parts: Vec<&str> = self.0.split(':').collect();
        match parts.as_slice() {
            [area, "*"] => *area == req.area,
            [area, access] => *area == req.area && *access == req.access.as_str(),
            [area, access, res] => {
                *area == req.area
                    && *access == req.access.as_str()
                    && req.resource.as_deref().is_some_and(|r| r == *res)
            }
            _ => false,
        }
    }

    /// Whether this scope is at most as broad as one of `held` (no privilege
    /// escalation when a scoped credential creates another one).
    pub fn within(&self, held: &[Scope]) -> bool {
        held.iter().any(|h| {
            if h.0 == "*" || h.0 == self.0 {
                return true;
            }
            let hp: Vec<&str> = h.0.split(':').collect();
            let sp: Vec<&str> = self.0.split(':').collect();
            match (hp.as_slice(), sp.as_slice()) {
                ([ha, "*"], [sa, ..]) => ha == sa && sp.len() >= 2,
                ([ha, hx], [sa, sx, _]) => ha == sa && hx == sx,
                _ => false,
            }
        })
    }
}

impl TryFrom<String> for Scope {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        Scope::parse(&s)
    }
}

impl From<Scope> for String {
    fn from(s: Scope) -> String {
        s.0
    }
}

/// Parses a list of scopes; at least one, at most 64.
pub fn parse_scopes(list: &[String]) -> Result<Vec<Scope>, String> {
    if list.is_empty() {
        return Err("at least one scope is required".into());
    }
    if list.len() > 64 {
        return Err("at most 64 scopes".into());
    }
    let mut out: Vec<Scope> = Vec::with_capacity(list.len());
    for s in list {
        let s = Scope::parse(s)?;
        if !out.contains(&s) {
            out.push(s);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(m: Method, p: &str) -> RequiredScope {
        required_scope(&m, p).unwrap()
    }

    #[test]
    fn paths_map_to_scopes() {
        assert_eq!(
            req(Method::GET, "/api/v1/collections/posts/records").to_string(),
            "collections:read:posts"
        );
        assert_eq!(
            req(Method::POST, "/api/v1/collections/posts/records").to_string(),
            "collections:write:posts"
        );
        assert_eq!(req(Method::GET, "/api/v1/collections").to_string(), "collections:read");
        assert_eq!(req(Method::POST, "/api/v1/sql").to_string(), "sql:write");
        assert_eq!(req(Method::DELETE, "/api/auth/sessions/x").to_string(), "auth:write");
        assert_eq!(req(Method::GET, "/api/system/info").to_string(), "system:read");
        assert!(required_scope(&Method::GET, "/health").is_none());
        assert_eq!(
            required_scope(&Method::GET, "/metrics")
                .map(|r| r.to_string())
                .as_deref(),
            Some("system:read"),
            "metrics need the system scope (FB-80)"
        );
        assert!(required_scope(&Method::GET, "/admin/").is_none());
    }

    #[test]
    fn grants() {
        let s = |x: &str| Scope::parse(x).unwrap();
        let r = req(Method::GET, "/api/v1/collections/posts/records");
        assert!(s("*").grants(&r));
        assert!(s("collections:*").grants(&r));
        assert!(s("collections:read").grants(&r));
        assert!(s("collections:read:posts").grants(&r));
        assert!(!s("collections:read:users").grants(&r));
        assert!(!s("collections:write").grants(&r));
        assert!(!s("sql:read").grants(&r));
        let w = req(Method::PATCH, "/api/v1/collections/posts/records/1");
        assert!(!s("collections:read").grants(&w));
        assert!(s("collections:write:posts").grants(&w));
    }

    #[test]
    fn parsing_and_escalation() {
        for bad in [
            "",
            "data",
            "data:delete",
            "Data:read",
            "a:read:b:c",
            "a:read:",
            ":read",
            "a b:read",
        ] {
            assert!(Scope::parse(bad).is_err(), "{bad}");
        }
        let held = parse_scopes(&["collections:read".into(), "auth:*".into()]).unwrap();
        assert!(Scope::parse("collections:read:posts").unwrap().within(&held));
        assert!(Scope::parse("auth:write").unwrap().within(&held));
        assert!(!Scope::parse("collections:write").unwrap().within(&held));
        assert!(!Scope::parse("*").unwrap().within(&held));
        assert!(parse_scopes(&[]).is_err());
    }
}
