//! A small JSON client for the emulator's own server. It talks plain HTTP to
//! the address the emulator bound, with the credential it holds, and never
//! reads `FORGEBASE_URL` / `FORGEBASE_TOKEN`: the emulator must not touch a
//! real server because of an environment variable that belongs to the app.

use std::time::Duration;

use anyhow::{Context, bail};
use serde_json::Value;

use crate::probe;

const TIMEOUT: Duration = Duration::from_secs(60);

/// A server address and a bearer credential.
#[derive(Debug, Clone)]
pub struct Api {
    /// `host:port` to dial.
    pub addr: String,
    /// API key or access token.
    pub token: Option<String>,
}

impl Api {
    /// A client for `addr` with an optional bearer token.
    pub fn new(addr: impl Into<String>, token: Option<String>) -> Self {
        Self {
            addr: addr.into(),
            token,
        }
    }

    /// One request; returns the status and the parsed body (`Null` when empty or not JSON).
    pub async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> anyhow::Result<(u16, Value)> {
        let raw = body.map(serde_json::to_vec).transpose()?;
        let reply = probe::request(&self.addr, method, path, self.token.as_deref(), raw.as_deref(), TIMEOUT)
            .await
            .with_context(|| format!("cannot reach the dev server at {}", self.addr))?;
        let parsed = if reply.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&reply.body).unwrap_or(Value::Null)
        };
        Ok((reply.status, parsed))
    }

    /// Like [`call`](Self::call), but a non-2xx answer is an error carrying the server's code and message.
    pub async fn ok(&self, method: &str, path: &str, body: Option<&Value>) -> anyhow::Result<Value> {
        let (status, v) = self.call(method, path, body).await?;
        if (200..300).contains(&status) {
            return Ok(v);
        }
        bail!("{}", describe(status, &v))
    }
}

/// `code: message (field: reason, ...)` of an error answer.
pub fn describe(status: u16, v: &Value) -> String {
    let e = &v["error"];
    let mut out = format!(
        "{}: {}",
        e["code"].as_str().unwrap_or("error"),
        e["message"].as_str().unwrap_or("the server refused the request")
    );
    if let Some(fields) = e["details"]["fields"].as_object().or_else(|| e["fields"].as_object()) {
        let list: Vec<String> = fields
            .iter()
            .map(|(k, v)| {
                format!(
                    "{k}: {}",
                    v["message"].as_str().or_else(|| v.as_str()).unwrap_or("invalid")
                )
            })
            .collect();
        if !list.is_empty() {
            out.push_str(&format!(" ({})", list.join(", ")));
        }
    }
    out.push_str(&format!(" (HTTP {status})"));
    out
}

/// Percent-encodes one path segment.
pub fn enc(s: &str) -> String {
    crate::functions::enc(s)
}
