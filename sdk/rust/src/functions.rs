//! Server functions: deploy, invoke, logs, versions, secrets.

use std::sync::Arc;

use base64::Engine as _;
use bytes::Bytes;
use serde::Serialize;
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::generated::{DeployRequest, Deployed, ExecutionLog, FunctionInfo, SecretInfo};
use crate::http::{Req, Transport, seg};

/// Server functions: `client.functions()`.
#[derive(Clone)]
pub struct FunctionsApi {
    pub(crate) t: Arc<Transport>,
}

fn list_of<T: serde::de::DeserializeOwned>(v: &Value, what: &str) -> Result<Vec<T>> {
    serde_json::from_value(v.get("items").cloned().unwrap_or(Value::Array(Vec::new())))
        .map_err(|e| Error::InvalidResponse(format!("{what}: {e}")))
}

impl FunctionsApi {
    /// Every function.
    pub async fn list(&self) -> Result<Vec<FunctionInfo>> {
        list_of(&self.t.value(Req::get("/api/v1/functions")).await?, "functions")
    }

    /// One function.
    pub async fn get(&self, name: &str) -> Result<FunctionInfo> {
        self.t.json(Req::get(format!("/api/v1/functions/{}", seg(name)))).await
    }

    /// Deploys a WebAssembly module as a new version. `wasm` is the module bytes (the SDK base64-encodes them).
    pub async fn deploy(&self, mut request: DeployRequest, wasm: &[u8]) -> Result<Deployed> {
        request.wasm = base64::engine::general_purpose::STANDARD.encode(wasm);
        self.t.json(Req::post("/api/v1/functions").json(&request)?).await
    }

    /// Deletes a function and its versions.
    pub async fn delete(&self, name: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!("/api/v1/functions/{}", seg(name))))
            .await
    }

    /// Invokes a function with a JSON payload and returns its JSON result (`Null` for an empty result).
    pub async fn invoke<B: Serialize + ?Sized>(&self, name: &str, payload: &B) -> Result<Value> {
        self.t
            .value(Req::post(format!("/api/v1/functions/{}/invoke", seg(name))).json(payload)?)
            .await
    }

    /// Invokes a function and returns the raw response bytes (for functions that do not answer JSON).
    pub async fn invoke_raw<B: Serialize + ?Sized>(&self, name: &str, payload: &B) -> Result<Bytes> {
        self.t
            .raw(Req::post(format!("/api/v1/functions/{}/invoke", seg(name))).json(payload)?)
            .await
    }

    /// Recent executions, newest first.
    pub async fn logs(&self, name: &str, limit: Option<u32>) -> Result<Vec<ExecutionLog>> {
        list_of(
            &self
                .t
                .value(Req::get(format!("/api/v1/functions/{}/logs", seg(name))).q_opt("limit", limit))
                .await?,
            "logs",
        )
    }

    /// Activates an older version (the previous one when `version` is `None`).
    pub async fn rollback(&self, name: &str, version: Option<i64>) -> Result<FunctionInfo> {
        self.t
            .json(
                Req::post(format!("/api/v1/functions/{}/rollback", seg(name))).json_value(json!({"version": version})),
            )
            .await
    }

    /// Secret names of a function (values are never returned).
    pub async fn secrets(&self, name: &str) -> Result<Vec<SecretInfo>> {
        list_of(
            &self
                .t
                .value(Req::get(format!("/api/v1/functions/{}/secrets", seg(name))))
                .await?,
            "secrets",
        )
    }

    /// Sets a secret.
    pub async fn put_secret(&self, name: &str, key: &str, value: &str) -> Result<SecretInfo> {
        self.t
            .json(
                Req::put(format!("/api/v1/functions/{}/secrets/{}", seg(name), seg(key)))
                    .json_value(json!({"value": value})),
            )
            .await
    }

    /// Deletes a secret.
    pub async fn delete_secret(&self, name: &str, key: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!(
                "/api/v1/functions/{}/secrets/{}",
                seg(name),
                seg(key)
            )))
            .await
    }
}
