//! Instance administration (`/api/system`, `/api/v1/system`): info, capabilities, features and
//! config, backups, direct database logins, Postgres extensions and the database topology
//! (replicas, shards, placement, rebalancing). `client.system()`.
//!
//! Every call needs an administrator. Destructive database operations ask for a typed
//! confirmation and, for an administrator with MFA, a second factor from the last ten minutes
//! (a permission error with code `mfa_step_up_required`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::generated::{
    Accepted, AddNode, AddShard, Added, Capabilities, CreateExtension, CreateSnapshot, DbLogin, ExtensionInfo,
    JoinToken, JoinTokenBody, LoginSecret, LoginSpec, Manifest, MoveBody, NodeDetail, NodeInfo, Operation,
    PlacementPage, PromoteBody, PromoteCheck, PruneResult, RebalanceBody, RebalancePlanBody, RebalancePlanView,
    Removed, Retention, RetiredInfo, RotateBody, SetInfo, SnapshotView, SystemConfigPatched, SystemFeatures,
    TopologyView, UpdateNode, UpdateShard, VerifyReport,
};
use crate::http::{Req, Transport, seg};

/// Filters of [`DatabaseTopologyApi::placements`].
#[derive(Debug, Clone, Default)]
pub struct PlacementsOptions {
    /// Only this set.
    pub set: Option<String>,
    /// Only this project.
    pub project: Option<String>,
    /// Tenant id contains.
    pub search: Option<String>,
    /// Page cursor (`next_cursor` of the previous page).
    pub cursor: Option<String>,
    /// 1 to 200 (default 50).
    pub limit: Option<u32>,
}

/// Instance administration: `client.system()`.
#[derive(Clone)]
pub struct SystemApi {
    pub(crate) t: Arc<Transport>,
}

impl SystemApi {
    /// Replicas, shards, tenant placement and rebalancing.
    #[must_use]
    pub fn database(&self) -> DatabaseTopologyApi {
        DatabaseTopologyApi { t: self.t.clone() }
    }
    /// Snapshots of the data directory.
    #[must_use]
    pub fn backups(&self) -> BackupsApi {
        BackupsApi { t: self.t.clone() }
    }
    /// Direct database logins (Postgres engine).
    #[must_use]
    pub fn db_logins(&self) -> DbLoginsApi {
        DbLoginsApi { t: self.t.clone() }
    }
    /// Postgres extensions.
    #[must_use]
    pub fn extensions(&self) -> ExtensionsApi {
        ExtensionsApi { t: self.t.clone() }
    }

    /// Name, version, data directory, uptime and database summary (including `database.capabilities`).
    pub async fn info(&self) -> Result<Value> {
        self.t.value(Req::get("/api/system/info")).await
    }

    /// Every feature of the server with the engine it runs on, whether that is a PostgreSQL
    /// extension, its state (`active`, `portable`, `off`, `native`, `unavailable`), configured mode,
    /// reason and limits (ADR-0034).
    pub async fn capabilities(&self) -> Result<Capabilities> {
        self.t.json(Req::get("/api/system/capabilities")).await
    }

    /// Every module, whether it is on, and its typed settings (secrets have `value: null`).
    pub async fn features(&self) -> Result<SystemFeatures> {
        self.t.json(Req::get("/api/system/features")).await
    }

    /// Writes settings to the config file (all keys validated first, one atomic write). `set` maps
    /// a setting key to its value; changes apply after a restart (`restart_required`). A bad key
    /// is a validation error with `details` per key.
    pub async fn update_config(&self, set: &Value) -> Result<SystemConfigPatched> {
        self.t
            .json(Req::patch("/api/system/config").json_raw(json!({"set": set})))
            .await
    }
}

/// Snapshots of the data directory: `client.system().backups()`.
#[derive(Clone)]
pub struct BackupsApi {
    t: Arc<Transport>,
}

impl BackupsApi {
    /// Snapshots.
    pub async fn list(&self) -> Result<Vec<SnapshotView>> {
        self.t.items(Req::get("/api/v1/system/backups")).await
    }

    /// Takes a snapshot now.
    pub async fn create(&self, o: &CreateSnapshot) -> Result<Manifest> {
        self.t.json(Req::post("/api/v1/system/backups").json(o)?).await
    }

    /// One snapshot.
    pub async fn get(&self, id: &str) -> Result<SnapshotView> {
        self.t
            .json(Req::get(format!("/api/v1/system/backups/{}", seg(id))))
            .await
    }

    /// Deletes a snapshot.
    pub async fn delete(&self, id: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!("/api/v1/system/backups/{}", seg(id))))
            .await
    }

    /// Re-reads every file of a snapshot and checks it against the manifest.
    pub async fn verify(&self, id: &str) -> Result<VerifyReport> {
        self.t
            .json(Req::post(format!("/api/v1/system/backups/{}/verify", seg(id))).json_raw(json!({})))
            .await
    }

    /// Deletes snapshots outside the retention; returns the ids removed.
    pub async fn prune(&self, r: &Retention) -> Result<Vec<String>> {
        let p: PruneResult = self.t.json(Req::post("/api/v1/system/backups/prune").json(r)?).await?;
        Ok(p.deleted)
    }
}

/// Direct database logins (Postgres engine): `client.system().db_logins()`.
#[derive(Clone)]
pub struct DbLoginsApi {
    t: Arc<Transport>,
}

impl DbLoginsApi {
    /// Logins.
    pub async fn list(&self) -> Result<Vec<DbLogin>> {
        self.t.items(Req::get("/api/v1/system/db-logins")).await
    }

    /// Creates a login; the password is in the result and shown once.
    pub async fn create(&self, spec: &LoginSpec) -> Result<LoginSecret> {
        self.t.json(Req::post("/api/v1/system/db-logins").json(spec)?).await
    }

    /// One login.
    pub async fn get(&self, name: &str) -> Result<DbLogin> {
        self.t
            .json(Req::get(format!("/api/v1/system/db-logins/{}", seg(name))))
            .await
    }

    /// A new password (shown once), optionally with a new expiry (RFC 3339).
    pub async fn rotate(&self, name: &str, valid_until: Option<&str>) -> Result<LoginSecret> {
        let body = RotateBody {
            valid_until: valid_until.map(str::to_owned),
        };
        self.t
            .json(Req::post(format!("/api/v1/system/db-logins/{}/rotate", seg(name))).json(&body)?)
            .await
    }

    /// Deletes a login.
    pub async fn delete(&self, name: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!("/api/v1/system/db-logins/{}", seg(name))))
            .await
    }
}

/// Postgres extensions: `client.system().extensions()`.
#[derive(Clone)]
pub struct ExtensionsApi {
    t: Arc<Transport>,
}

impl ExtensionsApi {
    /// Extensions and whether they are installed.
    pub async fn list(&self) -> Result<Vec<ExtensionInfo>> {
        self.t.items(Req::get("/api/v1/system/extensions")).await
    }

    /// Installs an extension.
    pub async fn install(&self, name: &str) -> Result<ExtensionInfo> {
        let body = CreateExtension { name: name.to_owned() };
        self.t.json(Req::post("/api/v1/system/extensions").json(&body)?).await
    }

    /// Drops an extension.
    pub async fn drop_extension(&self, name: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!("/api/v1/system/extensions/{}", seg(name))))
            .await
    }
}

/// Replicas, shards, tenant placement and rebalancing: `client.system().database()`.
#[derive(Clone)]
pub struct DatabaseTopologyApi {
    t: Arc<Transport>,
}

const D: &str = "/api/system/database";

impl DatabaseTopologyApi {
    /// Sets, nodes and running operations.
    pub async fn topology(&self) -> Result<TopologyView> {
        self.t.json(Req::get(format!("{D}/topology"))).await
    }

    /// One node with its replication lag history.
    pub async fn node(&self, id: &str) -> Result<NodeDetail> {
        self.t.json(Req::get(format!("{D}/nodes/{}", seg(id)))).await
    }

    /// Adds a replica. A local one is cloned by an operation (`operation`); an external one is
    /// registered at once (`node`).
    pub async fn add_node(&self, input: &AddNode) -> Result<Added> {
        self.t.json(Req::post(format!("{D}/nodes")).json(input)?).await
    }

    /// Changes a replica's share of reads (0 takes it out of rotation).
    pub async fn update_node(&self, id: &str, weight: i64) -> Result<NodeInfo> {
        self.t
            .json(Req::patch(format!("{D}/nodes/{}", seg(id))).json(&UpdateNode { weight })?)
            .await
    }

    /// Removes a replica; `confirm` is its name.
    pub async fn remove_node(&self, id: &str, confirm: &str) -> Result<String> {
        let r: Removed = self
            .t
            .json(Req::delete(format!("{D}/nodes/{}", seg(id))).q("confirm", confirm))
            .await?;
        Ok(r.removed)
    }

    /// Can this replica be promoted now, and how far behind is it?
    pub async fn promote_check(&self, id: &str) -> Result<PromoteCheck> {
        self.t
            .json(Req::get(format!("{D}/nodes/{}/promote-check", seg(id))))
            .await
    }

    /// Promotes a replica to primary (an operation); `confirm` is the node's name.
    pub async fn promote(&self, id: &str, confirm: &str, accept_data_loss: bool) -> Result<Operation> {
        let body = PromoteBody {
            confirm: confirm.to_owned(),
            accept_data_loss: accept_data_loss.then_some(true),
        };
        self.accepted(Req::post(format!("{D}/nodes/{}/promote", seg(id))).json(&body)?)
            .await
    }

    /// Re-clones a replica from the primary (an operation).
    pub async fn rebuild_node(&self, id: &str) -> Result<Operation> {
        self.accepted(Req::post(format!("{D}/nodes/{}/rebuild", seg(id))).json_raw(json!({})))
            .await
    }

    /// A token (shown once) and the command that joins a replica on another host.
    pub async fn create_join_token(&self, set: Option<&str>, role: Option<&str>) -> Result<JoinToken> {
        let body = JoinTokenBody {
            set: set.map(str::to_owned),
            role: role.map(str::to_owned),
        };
        self.t.json(Req::post(format!("{D}/join-tokens")).json(&body)?).await
    }

    /// Adds a shard (an operation).
    pub async fn add_shard(&self, input: &AddShard) -> Result<Operation> {
        self.accepted(Req::post(format!("{D}/shards")).json(input)?).await
    }

    /// Changes a shard: `active` / `draining`, weight, whether it takes new tenants.
    pub async fn update_shard(&self, id: &str, patch: &UpdateShard) -> Result<SetInfo> {
        self.t
            .json(Req::patch(format!("{D}/shards/{}", seg(id))).json(patch)?)
            .await
    }

    /// Removes an empty shard (a conflict with code `shard_not_empty` otherwise); `confirm` is its name.
    pub async fn remove_shard(&self, id: &str, confirm: &str) -> Result<String> {
        let r: Removed = self
            .t
            .json(Req::delete(format!("{D}/shards/{}", seg(id))).q("confirm", confirm))
            .await?;
        Ok(r.removed)
    }

    /// Where each tenant lives, one page (`next_cursor` continues).
    pub async fn placements(&self, o: &PlacementsOptions) -> Result<PlacementPage> {
        self.t
            .json(
                Req::get(format!("{D}/placements"))
                    .q_opt("set", o.set.as_deref())
                    .q_opt("project", o.project.as_deref())
                    .q_opt("q", o.search.as_deref())
                    .q_opt("cursor", o.cursor.as_deref())
                    .q_opt("limit", o.limit),
            )
            .await
    }

    /// Moves a tenant to another set online (an operation); `confirm` is the tenant id.
    pub async fn move_tenant(&self, tenant: &str, to: &str, confirm: &str) -> Result<Operation> {
        let body = MoveBody {
            to: to.to_owned(),
            confirm: confirm.to_owned(),
        };
        self.accepted(Req::post(format!("{D}/placements/{}/move", seg(tenant))).json(&body)?)
            .await
    }

    /// Plans a rebalance without running it.
    pub async fn rebalance_plan(&self, o: &RebalancePlanBody) -> Result<RebalancePlanView> {
        self.t.json(Req::post(format!("{D}/rebalance/plan")).json(o)?).await
    }

    /// Runs a stored plan (an operation); `confirm` is the plan id.
    pub async fn rebalance(&self, plan_id: &str, confirm: &str) -> Result<Operation> {
        let body = RebalanceBody {
            plan_id: plan_id.to_owned(),
            confirm: confirm.to_owned(),
        };
        self.accepted(Req::post(format!("{D}/rebalance")).json(&body)?).await
    }

    /// Data of removed shards still on disk.
    pub async fn retired(&self) -> Result<Vec<RetiredInfo>> {
        self.t.items(Req::get(format!("{D}/retired"))).await
    }

    /// Deletes the data of a removed shard; `confirm` is its node's name.
    pub async fn delete_retired(&self, id: &str, confirm: &str) -> Result<String> {
        let r: Removed = self
            .t
            .json(Req::delete(format!("{D}/retired/{}", seg(id))).q("confirm", confirm))
            .await?;
        Ok(r.removed)
    }

    /// Topology operations, newest first (`active`: pending and running only).
    pub async fn operations(&self, active: bool, limit: Option<u32>) -> Result<Vec<Operation>> {
        let r = Req::get(format!("{D}/operations")).q_opt("limit", limit);
        self.t.items(if active { r.q("active", "true") } else { r }).await
    }

    /// One operation.
    pub async fn operation(&self, id: &str) -> Result<Operation> {
        self.t.json(Req::get(format!("{D}/operations/{}", seg(id)))).await
    }

    /// Cancels an operation at its next safe point (a conflict with code `operation_not_cancellable` past it).
    pub async fn cancel_operation(&self, id: &str) -> Result<Operation> {
        self.t
            .json(Req::post(format!("{D}/operations/{}/cancel", seg(id))).json_raw(json!({})))
            .await
    }

    /// Polls an operation until it has `succeeded`, `failed`, been `cancelled` or died, and returns
    /// its final state. `Error::Config` after `timeout`.
    pub async fn wait_for_operation(
        &self,
        id: &str,
        interval: Duration,
        timeout: Option<Duration>,
    ) -> Result<Operation> {
        let deadline = timeout.map(|t| Instant::now() + t);
        loop {
            let op = self.operation(id).await?;
            if matches!(op.status.as_str(), "succeeded" | "failed" | "cancelled" | "dead") {
                return Ok(op);
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(Error::Config(format!(
                    "operation {id} is still {} after the timeout",
                    op.status
                )));
            }
            tokio::time::sleep(interval).await;
        }
    }

    async fn accepted(&self, r: Req) -> Result<Operation> {
        let a: Accepted = self.t.json(r).await?;
        Ok(a.operation)
    }
}
