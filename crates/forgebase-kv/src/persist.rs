//! SQLite write-behind persistence (ADR-0004-kv).
//!
//! Values are serialised as JSON with binary parts in base64, one row per key
//! in `_forgebase_kv_entries`. The store tracks which keys changed; the
//! service writes those in one transaction on an interval and at shutdown.

use base64::engine::general_purpose::STANDARD;
use forgebase_db::{Backend, Statement, TxMode, Value};
use serde::{Deserialize, Serialize};

use crate::data::{Data, HashV, Kind, ListV, SetV, StreamId, StreamV, ZSetV};
use crate::error::{KvError, KvResult};
use crate::store::{Entry, KeyId, Scope};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct B(#[serde(with = "b64")] Vec<u8>);

mod b64 {
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::STANDARD.encode(v))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        super::STANDARD.decode(s).map_err(serde::de::Error::custom)
    }
}

type Pair = (B, B);

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "t")]
enum Persisted {
    Str {
        v: B,
    },
    Hash {
        v: Vec<Pair>,
    },
    List {
        v: Vec<B>,
    },
    Set {
        v: Vec<B>,
    },
    // Scores are stored as raw f64 bits so infinities survive JSON.
    ZSet {
        v: Vec<(B, u64)>,
    },
    Stream {
        last: (u64, u64),
        v: Vec<((u64, u64), Vec<Pair>)>,
    },
}

fn encode_data(d: &Data) -> Persisted {
    match d {
        Data::Str(s) => Persisted::Str { v: B(s.clone()) },
        Data::Hash(h) => Persisted::Hash {
            v: h.map.iter().map(|(k, v)| (B(k.clone()), B(v.clone()))).collect(),
        },
        Data::List(l) => Persisted::List {
            v: l.items.iter().map(|i| B(i.clone())).collect(),
        },
        Data::Set(s) => Persisted::Set {
            v: s.set.iter().map(|i| B(i.clone())).collect(),
        },
        Data::ZSet(z) => Persisted::ZSet {
            v: z.order.iter().map(|(sc, m)| (B(m.clone()), sc.0.to_bits())).collect(),
        },
        Data::Stream(s) => Persisted::Stream {
            last: (s.last.ms, s.last.seq),
            v: s.entries
                .iter()
                .map(|(id, f)| {
                    (
                        (id.ms, id.seq),
                        f.iter().map(|(k, v)| (B(k.clone()), B(v.clone()))).collect(),
                    )
                })
                .collect(),
        },
    }
}

fn decode_data(p: Persisted) -> Data {
    match p {
        Persisted::Str { v } => Data::Str(v.0),
        Persisted::Hash { v } => {
            let mut h = HashV::default();
            for (k, val) in v {
                h.set(k.0, val.0);
            }
            Data::Hash(h)
        }
        Persisted::List { v } => {
            let mut l = ListV::default();
            for i in v {
                l.push_back(i.0);
            }
            Data::List(l)
        }
        Persisted::Set { v } => {
            let mut s = SetV::default();
            for i in v {
                s.insert(i.0);
            }
            Data::Set(s)
        }
        Persisted::ZSet { v } => {
            let mut z = ZSetV::default();
            for (m, bits) in v {
                z.set(m.0, f64::from_bits(bits));
            }
            Data::ZSet(z)
        }
        Persisted::Stream { last, v } => {
            let mut s = StreamV::default();
            for ((ms, seq), f) in v {
                s.insert(
                    StreamId { ms, seq },
                    f.into_iter().map(|(k, val)| (k.0, val.0)).collect(),
                );
            }
            s.last = StreamId {
                ms: last.0,
                seq: last.1,
            };
            Data::Stream(s)
        }
    }
}

/// One row to write: `None` deletes the key.
pub(crate) type Change = (KeyId, Option<Entry>);

fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// Writes changed keys in one transaction.
pub(crate) async fn write(backend: &dyn Backend, changes: &[Change]) -> KvResult<()> {
    if changes.is_empty() {
        return Ok(());
    }
    let mut tx = backend.begin(TxMode::Normal).await?;
    let result = async {
        for (id, entry) in changes {
            let stmt = match entry {
                Some(e) => {
                    let json = serde_json::to_string(&encode_data(&e.data))
                        .map_err(|e| KvError::Storage(format!("encode: {e}")))?;
                    Statement::new(
                        "INSERT INTO _forgebase_kv_entries (tenant, namespace, key, type, data, expires_at, updated_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                         ON CONFLICT (tenant, namespace, key) DO UPDATE SET \
                         type = excluded.type, data = excluded.data, \
                         expires_at = excluded.expires_at, updated_at = excluded.updated_at",
                        vec![
                            Value::from(id.scope.tenant.as_str()),
                            Value::from(id.scope.ns.as_str()),
                            Value::from(id.key.as_str()),
                            Value::from(e.data.kind().name()),
                            Value::Text(json),
                            e.expires_at.map_or(Value::Null, |t| Value::Integer(to_i64(t))),
                            Value::Integer(to_i64(e.updated_at)),
                        ],
                    )
                }
                None => Statement::new(
                    "DELETE FROM _forgebase_kv_entries WHERE tenant = ?1 AND namespace = ?2 AND key = ?3",
                    vec![
                        Value::from(id.scope.tenant.as_str()),
                        Value::from(id.scope.ns.as_str()),
                        Value::from(id.key.as_str()),
                    ],
                ),
            };
            tx.execute(&stmt).await?;
        }
        Ok::<(), KvError>(())
    }
    .await;
    match result {
        Ok(()) => {
            tx.commit().await?;
            Ok(())
        }
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e)
        }
    }
}

/// A persisted key read back at start-up.
pub(crate) struct Loaded {
    pub(crate) id: KeyId,
    pub(crate) data: Data,
    pub(crate) expires_at: Option<u64>,
    pub(crate) updated_at: u64,
}

/// Reads every persisted key, oldest write first (so recently written keys
/// are the most recently used after loading). Rows that do not decode are
/// skipped with a warning.
pub(crate) async fn read_all(backend: &dyn Backend) -> KvResult<Vec<Loaded>> {
    let rows = backend
        .fetch_all(&Statement::new(
            "SELECT tenant, namespace, key, type, data, expires_at, updated_at \
             FROM _forgebase_kv_entries ORDER BY updated_at ASC",
            vec![],
        ))
        .await?;
    let mut out = Vec::with_capacity(rows.rows.len());
    for r in &rows.rows {
        let (Some(tenant), Some(ns), Some(key), Some(kind), Some(data)) = (
            r.first().and_then(Value::as_str),
            r.get(1).and_then(Value::as_str),
            r.get(2).and_then(Value::as_str),
            r.get(3).and_then(Value::as_str),
            r.get(4).and_then(Value::as_str),
        ) else {
            continue;
        };
        let decoded = serde_json::from_str::<Persisted>(data)
            .ok()
            .map(decode_data)
            .filter(|d| Kind::from_name(kind).is_some_and(|k| k == d.kind()));
        let Some(data) = decoded else {
            tracing::warn!(tenant, ns, key, "skipping undecodable kv row");
            continue;
        };
        out.push(Loaded {
            id: KeyId {
                scope: Scope {
                    tenant: tenant.to_owned(),
                    ns: ns.to_owned(),
                },
                key: key.to_owned(),
            },
            data,
            expires_at: r.get(5).and_then(Value::as_i64).and_then(|v| u64::try_from(v).ok()),
            updated_at: r
                .get(6)
                .and_then(Value::as_i64)
                .and_then(|v| u64::try_from(v).ok())
                .unwrap_or(0),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_type_round_trips_including_infinite_scores() {
        let mut z = ZSetV::default();
        z.set(b"a".to_vec(), f64::INFINITY);
        z.set(b"b".to_vec(), -1.5);
        let mut st = StreamV::default();
        st.insert(StreamId { ms: 5, seq: 1 }, vec![(b"f".to_vec(), vec![0, 255])]);
        let mut h = HashV::default();
        h.set(b"k".to_vec(), vec![1, 2, 3]);
        let mut l = ListV::default();
        l.push_back(vec![0xff]);
        let mut s = SetV::default();
        s.insert(b"m".to_vec());
        for d in [
            Data::Str(vec![0, 1, 2, 255]),
            Data::Hash(h),
            Data::List(l),
            Data::Set(s),
            Data::ZSet(z),
            Data::Stream(st),
        ] {
            let json = serde_json::to_string(&encode_data(&d)).unwrap();
            let back = decode_data(serde_json::from_str(&json).unwrap());
            assert_eq!(back.kind(), d.kind());
            assert_eq!(back.mem(), d.mem(), "memory accounting survives a round trip");
            assert_eq!(
                serde_json::to_string(&encode_data(&back)).unwrap(),
                json,
                "stable encoding"
            );
        }
    }
}
