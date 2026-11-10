//! Record hooks.
//!
//! * **Before** (`before_create`, `before_update`, `before_delete`):
//!   [`HookGuard`] is the data core's [`WriteGuard`]. It runs after the
//!   caller's write was authorised and before its transaction starts, so a
//!   slow hook never holds the single writer. Each hook function of the
//!   collection and event runs in name order; the first that answers
//!   `{"allow": false, "reason": "..."}` rejects the write (`400
//!   hook_rejected`). A hook that fails (trap, fuel, memory, timeout, bad
//!   output, busy) also rejects it (`400 hook_failed`): fail closed.
//! * **After** (`after_create`, ...): [`FunctionService::run_after_hooks`]
//!   tails the realtime outbox (FB-40) from a stored cursor, so a hook runs
//!   only for committed changes, at least once, in commit order, also for
//!   changes made while the server was down. A failing after-hook is
//!   recorded in the execution log and not retried. A function's own writes
//!   do not trigger its own after-hooks (no self-loops).
//!
//! Hook input: `{"event", "collection", "record_id", "data", "auth"}` before
//! (`data` is the create body or the update patch), `{"event", "collection",
//! "record_id", "record", "actor", "seq"}` after (`record` is the stored row
//! after the change, before it for deletes).
//! Writes made by the system principal (migrations, internal jobs) skip hooks.

use std::sync::{Arc, Weak};

use forgebase_data::schema::Collection;
use forgebase_data::{ChangeOp, GuardRejection, PendingWrite, Principal, WriteGuard};
use forgebase_db::{BoxFuture, Value};
use forgebase_realtime::Event;
use serde_json::{Map, Value as Json, json};

use crate::error::FunctionError;
use crate::model::{HookEvent, Trigger};
use crate::{FunctionService, Inner, identity};

/// The before-hook guard installed on the data service.
#[derive(Debug)]
pub struct HookGuard {
    inner: Weak<Inner>,
}

impl HookGuard {
    pub(crate) fn new(svc: &FunctionService) -> Self {
        Self {
            inner: Arc::downgrade(&svc.inner),
        }
    }

    fn service(&self) -> Option<FunctionService> {
        self.inner.upgrade().map(|inner| FunctionService { inner })
    }
}

/// `{"type", "id"}` of a caller, for hook and HTTP inputs.
pub(crate) fn auth_json(p: &Principal) -> Json {
    json!({"type": p.kind(), "id": p.id()})
}

/// Whether a before-hook output rejects the write: `Some(reason)`.
pub(crate) fn rejection(output: Option<&[u8]>) -> Result<Option<String>, String> {
    let Some(bytes) = output.filter(|b| !b.is_empty()) else {
        return Ok(None);
    };
    let v: Json = serde_json::from_slice(bytes).map_err(|_| "the hook's output is not JSON".to_owned())?;
    match v.get("allow") {
        None | Some(Json::Bool(true)) => Ok(None),
        Some(Json::Bool(false)) => Ok(Some(
            v.get("reason")
                .and_then(Json::as_str)
                .unwrap_or("rejected by a hook")
                .chars()
                .take(500)
                .collect(),
        )),
        Some(_) => Err("the hook's \"allow\" must be true or false".to_owned()),
    }
}

impl WriteGuard for HookGuard {
    fn applies(&self, op: ChangeOp, collection: &Collection) -> bool {
        self.service().is_some_and(|s| {
            s.registry()
                .hooks
                .contains_key(&(collection.name.clone(), HookEvent::of(op, true)))
        })
    }

    fn check(&self, write: PendingWrite) -> BoxFuture<'static, Result<(), GuardRejection>> {
        let svc = self.service();
        Box::pin(async move {
            let Some(svc) = svc else { return Ok(()) };
            let event = HookEvent::of(write.op, true);
            let names = svc
                .registry()
                .hooks
                .get(&(write.collection.name.clone(), event))
                .cloned()
                .unwrap_or_default();
            let mut input = json!({
                "event": event.as_str(),
                "collection": write.collection.name,
                "record_id": write.record_id,
                "auth": auth_json(&write.ctx.principal),
            });
            if let Some(d) = write.data {
                input["data"] = d;
            }
            let bytes = input.to_string().into_bytes();
            for name in names {
                let Ok(entry) = svc.entry(&name) else { continue };
                let mut verdict: Option<Result<Option<String>, String>> = None;
                let res = svc
                    .execute(
                        &entry,
                        None,
                        Trigger::Hook,
                        bytes.clone(),
                        write.ctx.request_id.clone(),
                        // JavaScript hooks act as the writer.
                        Some(write.ctx.clone()),
                        |out| {
                            let v = rejection(out);
                            let status = match &v {
                                Ok(Some(reason)) => Some(("rejected", reason.clone())),
                                Ok(None) => None,
                                Err(m) => Some(("bad_output", m.clone())),
                            };
                            verdict = Some(v);
                            status
                        },
                    )
                    .await;
                match (res, verdict) {
                    (Ok(_), Some(Ok(None))) => {}
                    (Ok(_), Some(Ok(Some(reason)))) => {
                        return Err(GuardRejection {
                            code: "hook_rejected",
                            message: format!("{name}: {reason}"),
                        });
                    }
                    (Ok(_), Some(Err(m))) => {
                        return Err(GuardRejection {
                            code: "hook_failed",
                            message: format!("{name}: {m}"),
                        });
                    }
                    (Ok(_), None) => {}
                    (Err(e), _) => {
                        let message = match &e {
                            FunctionError::Internal(_) => format!("{name}: internal error"),
                            other => format!("{name}: {other}"),
                        };
                        return Err(GuardRejection {
                            code: "hook_failed",
                            message,
                        });
                    }
                }
            }
            Ok(())
        })
    }
}

/// A stored row as a record object (`_data` documents merged in).
fn record_json(ev: &Event) -> Json {
    let mut out = Map::new();
    for (c, v) in ev.snapshot.columns.iter().zip(&ev.snapshot.values) {
        let j = match v {
            Value::Null => Json::Null,
            Value::Integer(i) => json!(i),
            Value::Real(f) => json!(f),
            Value::Text(s) if c == "_data" => match serde_json::from_str::<Json>(s) {
                Ok(Json::Object(m)) => {
                    for (k, v) in m {
                        out.entry(k).or_insert(v);
                    }
                    continue;
                }
                _ => Json::String(s.clone()),
            },
            Value::Text(s) => Json::String(s.clone()),
            Value::Blob(_) => continue,
        };
        out.insert(c.clone(), j);
    }
    Json::Object(out)
}

/// Runs the after-hooks of one committed event; returns how many ran.
pub(crate) async fn after(svc: &FunctionService, ev: &Event) -> usize {
    let event = HookEvent::of(ev.op, false);
    let Some(names) = svc.registry().hooks.get(&(ev.collection.clone(), event)).cloned() else {
        return 0;
    };
    let input = json!({
        "event": event.as_str(),
        "collection": ev.collection,
        "record_id": ev.record_id,
        "record": record_json(ev),
        "actor": ev.actor,
        "seq": ev.seq,
    });
    let mut runs = 0;
    for name in names {
        if ev.actor == identity(&name).actor() {
            continue;
        }
        runs += 1;
        if let Err(e) = svc.invoke(&name, Trigger::Hook, &input, None).await {
            tracing::warn!(function = %name, seq = ev.seq, error = %e, "after-hook failed");
        }
    }
    runs
}
