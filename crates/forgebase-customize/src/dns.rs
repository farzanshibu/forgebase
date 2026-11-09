//! DNS TXT lookups for custom-domain verification.
//!
//! The verifier only needs TXT records, so it takes a [`DnsResolver`]: the
//! production [`DohResolver`] asks a DNS-over-HTTPS JSON endpoint (default
//! Cloudflare) over the outbound HTTPS client of `forgebase-auth`, which needs
//! no DNS library and works behind an egress proxy; [`StaticResolver`] answers
//! from memory for tests.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use forgebase_auth::social::http_client::{HttpClient, HttpRequest};
use forgebase_db::backend::BoxFuture;

/// Why a lookup failed (a name that has no TXT records is not a failure).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct DnsError(pub String);

/// Looks up TXT records.
pub trait DnsResolver: Send + Sync + fmt::Debug {
    /// The TXT strings at `name` (each record's character-strings joined).
    /// An absent name or record set is `Ok(vec![])`.
    fn txt<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<Vec<String>, DnsError>>;
}

/// An in-memory resolver for tests and air-gapped setups.
#[derive(Debug, Clone, Default)]
pub struct StaticResolver {
    records: Arc<Mutex<HashMap<String, Vec<String>>>>,
    failing: Arc<Mutex<bool>>,
}

impl StaticResolver {
    /// An empty resolver.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a TXT record at `name`.
    pub fn add_txt(&self, name: &str, value: &str) {
        if let Ok(mut m) = self.records.lock() {
            m.entry(name.to_ascii_lowercase()).or_default().push(value.to_owned());
        }
    }

    /// Removes every record at `name`.
    pub fn clear(&self, name: &str) {
        if let Ok(mut m) = self.records.lock() {
            m.remove(&name.to_ascii_lowercase());
        }
    }

    /// Makes every lookup fail (a resolver outage).
    pub fn set_failing(&self, failing: bool) {
        if let Ok(mut f) = self.failing.lock() {
            *f = failing;
        }
    }
}

impl DnsResolver for StaticResolver {
    fn txt<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<Vec<String>, DnsError>> {
        Box::pin(async move {
            if self.failing.lock().map(|f| *f).unwrap_or(false) {
                return Err(DnsError("resolver unavailable".into()));
            }
            Ok(self
                .records
                .lock()
                .ok()
                .and_then(|m| m.get(&name.to_ascii_lowercase()).cloned())
                .unwrap_or_default())
        })
    }
}

/// DNS-over-HTTPS (`application/dns-json`) resolver.
#[derive(Debug, Clone)]
pub struct DohResolver {
    client: Arc<dyn HttpClient>,
    endpoint: String,
}

impl DohResolver {
    /// Cloudflare's JSON endpoint.
    pub const CLOUDFLARE: &'static str = "https://cloudflare-dns.com/dns-query";

    /// A resolver using `client` against `endpoint`.
    pub fn new(client: Arc<dyn HttpClient>, endpoint: impl Into<String>) -> Self {
        Self {
            client,
            endpoint: endpoint.into(),
        }
    }
}

/// Strips the quotes of a DoH TXT `data` field and joins its strings.
fn unquote(data: &str) -> String {
    let t = data.trim();
    if !t.starts_with('"') {
        return t.to_owned();
    }
    let mut out = String::new();
    let mut in_q = false;
    let mut esc = false;
    for c in t.chars() {
        if esc {
            out.push(c);
            esc = false;
        } else if in_q && c == '\\' {
            esc = true;
        } else if c == '"' {
            in_q = !in_q;
        } else if in_q {
            out.push(c);
        }
    }
    out
}

impl DnsResolver for DohResolver {
    fn txt<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<Vec<String>, DnsError>> {
        Box::pin(async move {
            let mut req = HttpRequest::get(format!("{}?name={name}&type=TXT", self.endpoint));
            req.headers.push(("accept".into(), "application/dns-json".into()));
            let res = self
                .client
                .send(req)
                .await
                .map_err(|e| DnsError(format!("DNS-over-HTTPS request failed: {e}")))?;
            if res.status != 200 {
                return Err(DnsError(format!("DNS-over-HTTPS answered {}", res.status)));
            }
            let j = res.json().map_err(|e| DnsError(e.to_string()))?;
            match j.get("Status").and_then(serde_json::Value::as_i64) {
                Some(0) => {}
                // NXDOMAIN: the name has no records.
                Some(3) => return Ok(Vec::new()),
                other => return Err(DnsError(format!("DNS status {other:?}"))),
            }
            Ok(j.get("Answer")
                .and_then(serde_json::Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter(|r| r.get("type").and_then(serde_json::Value::as_i64) == Some(16))
                        .filter_map(|r| r.get("data").and_then(serde_json::Value::as_str))
                        .map(unquote)
                        .collect()
                })
                .unwrap_or_default())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::unquote;

    #[test]
    fn unquotes_txt_data() {
        assert_eq!(unquote(r#""forgebase-verify=abc""#), "forgebase-verify=abc");
        assert_eq!(unquote(r#""forgebase-ve" "rify=abc""#), "forgebase-verify=abc");
        assert_eq!(unquote("plain"), "plain");
    }
}
