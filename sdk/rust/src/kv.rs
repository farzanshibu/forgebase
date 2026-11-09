//! Key-value store: strings, TTLs, counters, hashes, lists, sets, sorted sets, streams, pub/sub.

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{Value, json};

use crate::error::Result;
use crate::generated::{KeyValue, KvStats};
use crate::http::{Req, Transport, seg};

/// A KV value: text, or bytes (sent as `{"base64": ...}`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KvValue {
    /// UTF-8 text.
    Text(String),
    /// Arbitrary bytes.
    Bytes(Vec<u8>),
}

impl KvValue {
    fn to_json(&self) -> Value {
        match self {
            Self::Text(s) => Value::String(s.clone()),
            Self::Bytes(b) => json!({"base64": base64::engine::general_purpose::STANDARD.encode(b)}),
        }
    }

    /// Decodes a value the server returned: a string, or `{"base64": "..."}`.
    #[must_use]
    pub fn from_json(v: &Value) -> Option<Self> {
        match v {
            Value::String(s) => Some(Self::Text(s.clone())),
            Value::Object(m) => {
                let b = m.get("base64")?.as_str()?;
                base64::engine::general_purpose::STANDARD
                    .decode(b)
                    .ok()
                    .map(Self::Bytes)
            }
            _ => None,
        }
    }
}

impl From<&str> for KvValue {
    fn from(s: &str) -> Self {
        Self::Text(s.to_owned())
    }
}
impl From<String> for KvValue {
    fn from(s: String) -> Self {
        Self::Text(s)
    }
}
impl From<Vec<u8>> for KvValue {
    fn from(b: Vec<u8>) -> Self {
        Self::Bytes(b)
    }
}
impl From<&[u8]> for KvValue {
    fn from(b: &[u8]) -> Self {
        Self::Bytes(b.to_vec())
    }
}

/// Options for [`Kv::set`].
#[derive(Debug, Clone, Default)]
pub struct SetOptions {
    /// Expire after this many milliseconds.
    pub ttl_ms: Option<u64>,
    /// Only set when the key does not exist.
    pub nx: bool,
    /// Only set when the key exists.
    pub xx: bool,
    /// Keep the existing TTL.
    pub keep_ttl: bool,
}

/// One KV namespace: `client.kv("cache")`.
#[derive(Clone)]
pub struct Kv {
    pub(crate) t: Arc<Transport>,
    pub(crate) ns: String,
}

impl Kv {
    fn base(&self) -> String {
        format!("/api/v1/kv/{}", seg(&self.ns))
    }

    /// Sets a string value. The reply says whether it was set (`nx` / `xx` can refuse).
    pub async fn set(&self, key: &str, value: impl Into<KvValue>, o: &SetOptions) -> Result<Value> {
        let body =
            json!({"value": value.into().to_json(), "ttl_ms": o.ttl_ms, "nx": o.nx, "xx": o.xx, "keepttl": o.keep_ttl});
        self.t
            .value(Req::put(format!("{}/strings/{}", self.base(), seg(key))).json_value(body))
            .await
    }

    /// Reads a key of any type: type, size, TTL and the value in the server's JSON shape.
    /// A missing key is a not-found error; see [`get_value`](Self::get_value) for an `Option`.
    pub async fn get(&self, key: &str) -> Result<KeyValue> {
        self.t
            .json(Req::get(format!("{}/keys/{}", self.base(), seg(key))))
            .await
    }

    /// A string key's value, `None` when the key does not exist.
    pub async fn get_value(&self, key: &str) -> Result<Option<KvValue>> {
        match self.get(key).await {
            Ok(kv) => Ok(KvValue::from_json(&kv.value)),
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// A string key's value as text, `None` when missing or not UTF-8 text.
    pub async fn get_string(&self, key: &str) -> Result<Option<String>> {
        Ok(match self.get_value(key).await? {
            Some(KvValue::Text(s)) => Some(s),
            Some(KvValue::Bytes(b)) => String::from_utf8(b).ok(),
            None => None,
        })
    }

    /// Deletes a key.
    pub async fn delete(&self, key: &str) -> Result<Value> {
        self.t
            .value(Req::delete(format!("{}/keys/{}", self.base(), seg(key))))
            .await
    }

    /// Adds `by` (default 1) to an integer string key and returns the reply (the new value).
    pub async fn incr(&self, key: &str, by: Option<i64>) -> Result<Value> {
        self.t
            .value(Req::post(format!("{}/strings/{}/incr", self.base(), seg(key))).json_value(json!({"by": by})))
            .await
    }

    /// Sets a TTL in milliseconds.
    pub async fn expire(&self, key: &str, ttl_ms: u64) -> Result<Value> {
        self.t
            .value(Req::put(format!("{}/keys/{}/ttl", self.base(), seg(key))).json_value(json!({"ttl_ms": ttl_ms})))
            .await
    }

    /// Removes a TTL.
    pub async fn persist(&self, key: &str) -> Result<Value> {
        self.t
            .value(Req::delete(format!("{}/keys/{}/ttl", self.base(), seg(key))))
            .await
    }

    /// One page of keys: `(keys, next_cursor)`; start with cursor `"0"`, the scan is done when the cursor is `"0"` again.
    pub async fn scan(&self, cursor: &str, pattern: Option<&str>, count: Option<u32>) -> Result<(Vec<String>, String)> {
        let v = self
            .t
            .value(
                Req::get(format!("{}/keys", self.base()))
                    .q("cursor", cursor)
                    .q_opt("match", pattern)
                    .q_opt("count", count),
            )
            .await?;
        let keys = v
            .get("keys")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|k| k.as_str().map(str::to_owned)).collect())
            .unwrap_or_default();
        let next = v.get("cursor").and_then(Value::as_str).unwrap_or("0").to_owned();
        Ok((keys, next))
    }

    /// Reads many string keys at once.
    pub async fn mget(&self, keys: &[&str]) -> Result<Value> {
        self.t
            .value(
                Req::post(format!("{}/mget", self.base()))
                    .json_value(json!({"keys": keys}))
                    .idempotent(),
            )
            .await
    }

    /// Sets many string keys at once.
    pub async fn mset(&self, entries: &[(&str, KvValue)]) -> Result<Value> {
        let e: Vec<Value> = entries
            .iter()
            .map(|(k, v)| json!({"key": k, "value": v.to_json()}))
            .collect();
        self.t
            .value(Req::post(format!("{}/mset", self.base())).json_value(json!({"entries": e})))
            .await
    }

    /// Sets hash fields (`{"field": "value"}`).
    pub async fn hash_set(&self, key: &str, fields: Value) -> Result<Value> {
        self.t
            .value(Req::post(format!("{}/hashes/{}/set", self.base(), seg(key))).json_value(json!({"fields": fields})))
            .await
    }

    /// Adds to a hash field.
    pub async fn hash_incr(&self, key: &str, field: &str, by: Option<i64>) -> Result<Value> {
        self.t
            .value(
                Req::post(format!("{}/hashes/{}/incr", self.base(), seg(key)))
                    .json_value(json!({"field": field, "by": by})),
            )
            .await
    }

    /// Deletes hash fields.
    pub async fn hash_delete(&self, key: &str, fields: &[&str]) -> Result<Value> {
        self.t
            .value(
                Req::post(format!("{}/hashes/{}/delete", self.base(), seg(key))).json_value(json!({"fields": fields})),
            )
            .await
    }

    /// Pushes values on the `left` or `right` of a list.
    pub async fn list_push(&self, key: &str, left: bool, values: &[KvValue]) -> Result<Value> {
        let v: Vec<Value> = values.iter().map(KvValue::to_json).collect();
        let side = if left { "left" } else { "right" };
        self.t
            .value(
                Req::post(format!("{}/lists/{}/push", self.base(), seg(key)))
                    .json_value(json!({"side": side, "values": v})),
            )
            .await
    }

    /// Pops from the `left` or `right` of a list.
    pub async fn list_pop(&self, key: &str, left: bool, count: Option<u32>) -> Result<Value> {
        let side = if left { "left" } else { "right" };
        self.t
            .value(
                Req::post(format!("{}/lists/{}/pop", self.base(), seg(key)))
                    .json_value(json!({"side": side, "count": count})),
            )
            .await
    }

    /// Adds set members.
    pub async fn set_add(&self, key: &str, members: &[KvValue]) -> Result<Value> {
        let m: Vec<Value> = members.iter().map(KvValue::to_json).collect();
        self.t
            .value(Req::post(format!("{}/sets/{}/add", self.base(), seg(key))).json_value(json!({"members": m})))
            .await
    }

    /// Removes set members.
    pub async fn set_remove(&self, key: &str, members: &[KvValue]) -> Result<Value> {
        let m: Vec<Value> = members.iter().map(KvValue::to_json).collect();
        self.t
            .value(Req::post(format!("{}/sets/{}/remove", self.base(), seg(key))).json_value(json!({"members": m})))
            .await
    }

    /// Adds scored members to a sorted set.
    pub async fn zset_add(&self, key: &str, members: &[(KvValue, f64)]) -> Result<Value> {
        let m: Vec<Value> = members
            .iter()
            .map(|(m, s)| json!({"member": m.to_json(), "score": s}))
            .collect();
        self.t
            .value(Req::post(format!("{}/zsets/{}/add", self.base(), seg(key))).json_value(json!({"members": m})))
            .await
    }

    /// Removes sorted-set members.
    pub async fn zset_remove(&self, key: &str, members: &[KvValue]) -> Result<Value> {
        let m: Vec<Value> = members.iter().map(KvValue::to_json).collect();
        self.t
            .value(Req::post(format!("{}/zsets/{}/remove", self.base(), seg(key))).json_value(json!({"members": m})))
            .await
    }

    /// Appends an entry to a stream and returns its id.
    pub async fn stream_add(&self, key: &str, fields: Value, maxlen: Option<i64>) -> Result<Value> {
        self.t
            .value(
                Req::post(format!("{}/streams/{}/add", self.base(), seg(key)))
                    .json_value(json!({"fields": fields, "maxlen": maxlen})),
            )
            .await
    }

    /// Reads a stream range (`start` / `end` default to `-` / `+`).
    pub async fn stream_range(
        &self,
        key: &str,
        start: Option<&str>,
        end: Option<&str>,
        count: Option<u32>,
    ) -> Result<Value> {
        let r = Req::get(format!("{}/streams/{}/range", self.base(), seg(key)))
            .q_opt("start", start)
            .q_opt("end", end)
            .q_opt("count", count);
        self.t.value(r).await
    }

    /// Publishes a message to a pub/sub channel of this namespace.
    pub async fn publish(&self, channel: &str, message: impl Into<KvValue>) -> Result<Value> {
        self.t
            .value(
                Req::post(format!("{}/publish/{}", self.base(), seg(channel)))
                    .json_value(json!({"message": message.into().to_json()})),
            )
            .await
    }

    /// Runs one Redis-style command (`["INCRBY", "hits", "2"]`).
    pub async fn exec(&self, command: &[&str]) -> Result<Value> {
        self.t
            .value(Req::post(format!("{}/exec", self.base())).json_value(json!({"command": command})))
            .await
    }

    /// Deletes every key of the namespace.
    pub async fn flush(&self) -> Result<Value> {
        self.t.value(Req::delete(self.base())).await
    }

    /// Server-wide KV counters (memory, hits, evictions).
    pub async fn info(&self) -> Result<KvStats> {
        self.t.json(Req::get("/api/v1/kv/_info")).await
    }

    /// Namespaces that hold keys.
    pub async fn namespaces(&self) -> Result<Value> {
        self.t.value(Req::get("/api/v1/kv/_namespaces")).await
    }
}

impl Kv {
    /// The namespace name.
    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.ns
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_roundtrip() {
        assert_eq!(KvValue::from_json(&json!("hi")), Some(KvValue::Text("hi".into())));
        let b = KvValue::Bytes(vec![0, 255, 7]);
        assert_eq!(KvValue::from_json(&b.to_json()), Some(b));
        assert_eq!(KvValue::from_json(&json!(5)), None);
    }
}
