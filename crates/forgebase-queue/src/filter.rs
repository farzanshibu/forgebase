//! Subscription filters over message attributes.
//!
//! A filter is a JSON object. Each key names an attribute and holds a
//! condition; every condition must hold (AND):
//!
//! | Condition | Matches when the attribute |
//! |---|---|
//! | `"text"` | equals `text` |
//! | `["a", "b"]` | is one of the strings |
//! | `{"eq": "a"}` / `{"ne": "a"}` | equals / is absent or differs |
//! | `{"in": [...]}` / `{"not_in": [...]}` | is / is not one of the strings |
//! | `{"prefix": "a"}` | starts with `a` |
//! | `{"exists": true}` / `{"exists": false}` | is present / absent |
//!
//! An operator object may combine several operators (all must hold). The
//! reserved key `"$any"` holds a list of filters of which at least one must
//! match (OR). Messages a filter rejects are never delivered to the
//! subscription and count as acknowledged for it.

use std::collections::BTreeMap;

use serde_json::Value as Json;

/// Largest filter (serialized JSON bytes).
pub const MAX_FILTER_BYTES: usize = 8 * 1024;

/// A parsed filter.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    conds: Vec<(String, Cond)>,
    any: Vec<Filter>,
}

#[derive(Debug, Clone, PartialEq)]
enum Cond {
    Eq(String),
    Ne(String),
    In(Vec<String>),
    NotIn(Vec<String>),
    Prefix(String),
    Exists(bool),
}

fn strings(v: &Json, what: &str) -> Result<Vec<String>, String> {
    v.as_array()
        .filter(|a| !a.is_empty())
        .and_then(|a| a.iter().map(|s| s.as_str().map(str::to_owned)).collect::<Option<Vec<_>>>())
        .ok_or_else(|| format!("{what} must be a non-empty array of strings"))
}

fn text(v: &Json, what: &str) -> Result<String, String> {
    v.as_str().map(str::to_owned).ok_or_else(|| format!("{what} must be a string"))
}

impl Filter {
    /// Parses a filter (see the module documentation).
    pub fn parse(v: &Json) -> Result<Self, String> {
        if v.to_string().len() > MAX_FILTER_BYTES {
            return Err(format!("filter is larger than {MAX_FILTER_BYTES} bytes"));
        }
        Self::parse_at(v, 0)
    }

    fn parse_at(v: &Json, depth: usize) -> Result<Self, String> {
        if depth > 8 {
            return Err("filter is nested too deeply".into());
        }
        let Some(obj) = v.as_object() else {
            return Err("a filter must be an object of attribute conditions".into());
        };
        let mut f = Filter {
            conds: Vec::new(),
            any: Vec::new(),
        };
        for (key, cond) in obj {
            if key == "$any" {
                let list = cond
                    .as_array()
                    .filter(|a| !a.is_empty())
                    .ok_or("$any must be a non-empty array of filters")?;
                for sub in list {
                    f.any.push(Self::parse_at(sub, depth + 1)?);
                }
                continue;
            }
            if key.is_empty() || key.starts_with('$') {
                return Err(format!("{key:?} is not an attribute name"));
            }
            let what = format!("condition of {key:?}");
            match cond {
                Json::String(s) => f.conds.push((key.clone(), Cond::Eq(s.clone()))),
                Json::Array(_) => f.conds.push((key.clone(), Cond::In(strings(cond, &what)?))),
                Json::Object(ops) if !ops.is_empty() => {
                    for (op, arg) in ops {
                        let c = match op.as_str() {
                            "eq" => Cond::Eq(text(arg, &what)?),
                            "ne" => Cond::Ne(text(arg, &what)?),
                            "in" => Cond::In(strings(arg, &what)?),
                            "not_in" => Cond::NotIn(strings(arg, &what)?),
                            "prefix" => Cond::Prefix(text(arg, &what)?),
                            "exists" => Cond::Exists(arg.as_bool().ok_or_else(|| format!("{what}: exists takes a boolean"))?),
                            other => return Err(format!("{what}: unknown operator {other:?}")),
                        };
                        f.conds.push((key.clone(), c));
                    }
                }
                _ => return Err(format!("{what} must be a string, an array or an operator object")),
            }
        }
        Ok(f)
    }

    /// Whether a message with `attrs` passes.
    pub fn matches(&self, attrs: &BTreeMap<String, String>) -> bool {
        let conds = self.conds.iter().all(|(k, c)| {
            let v = attrs.get(k);
            match c {
                Cond::Eq(s) => v == Some(s),
                Cond::Ne(s) => v != Some(s),
                Cond::In(list) => v.is_some_and(|v| list.contains(v)),
                Cond::NotIn(list) => v.is_none_or(|v| !list.contains(v)),
                Cond::Prefix(p) => v.is_some_and(|v| v.starts_with(p.as_str())),
                Cond::Exists(want) => v.is_some() == *want,
            }
        });
        conds && (self.any.is_empty() || self.any.iter().any(|f| f.matches(attrs)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn attrs(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn conditions_and_any() {
        let f = Filter::parse(&json!({
            "type": "order",
            "region": ["eu", "us"],
            "trace": {"exists": false},
            "$any": [{"tier": {"prefix": "gold"}}, {"vip": "yes"}]
        }))
        .unwrap();
        assert!(f.matches(&attrs(&[("type", "order"), ("region", "eu"), ("tier", "gold-1")])));
        assert!(f.matches(&attrs(&[("type", "order"), ("region", "us"), ("vip", "yes")])));
        assert!(!f.matches(&attrs(&[("type", "order"), ("region", "us")])));
        assert!(!f.matches(&attrs(&[("type", "order"), ("region", "asia"), ("vip", "yes")])));
        assert!(!f.matches(&attrs(&[("type", "order"), ("region", "eu"), ("vip", "yes"), ("trace", "1")])));
    }

    #[test]
    fn bad_filters_are_rejected() {
        assert!(Filter::parse(&json!("type")).is_err());
        assert!(Filter::parse(&json!({"a": 1})).is_err());
        assert!(Filter::parse(&json!({"a": {"regex": "x"}})).is_err());
        assert!(Filter::parse(&json!({"a": []})).is_err());
        assert!(Filter::parse(&json!({"$all": []})).is_err());
    }
}
