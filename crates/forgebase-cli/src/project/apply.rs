//! Executing a plan (ADR-0037 §5): in dependency order, one resource at a
//! time, each checked against the server state the plan saw.
//!
//! - **Optimistic concurrency.** Before a change runs, the resource is read
//!   again. If the server no longer has the state the plan saw (another
//!   apply, the dashboard, the API), the change fails with
//!   `changed_since_plan` instead of overwriting it. A resource that already
//!   has the project's state is skipped, so a plan can be applied again.
//! - **Partial failure.** A failed change does not stop independent ones;
//!   changes that depend on it are skipped. The next run plans only what is
//!   still different, so it converges.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use base64::Engine as _;
use serde_json::{Map, Value, json};

use super::layout::Project;
use super::model::{self, Actual, Code, Desired, Extra, Kind};
use super::plan::{Action, Change, Plan};
use super::remote::{Remote, enc, is_not_found};

/// Options of `apply`.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Changes flagged destructive may run.
    pub allow_destructive: bool,
    /// Send every function secret again (values cannot be compared).
    pub update_secrets: bool,
    /// File for secrets the server issues (`NAME=value`).
    pub secrets_out: Option<PathBuf>,
}

/// What an apply did.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Changes made.
    pub applied: usize,
    /// Changes found already in place.
    pub in_place: usize,
    /// Changes skipped because a dependency failed.
    pub skipped: usize,
    /// Failures: `kind:name` and the error.
    pub failed: Vec<(String, String)>,
    /// Secrets the server issued: variable name -> value.
    pub secrets: BTreeMap<String, String>,
    /// Issued secrets that were not exported (no `--secrets-out`).
    pub unsaved_secrets: Vec<String>,
}

/// Refuses destructive changes without `--allow-destructive` (before anything runs).
pub fn check_destructive(plan: &Plan, opts: &Options) -> anyhow::Result<()> {
    let flagged: Vec<String> = plan
        .changes
        .iter()
        .filter(|c| !c.destructive.is_empty())
        .map(|c| format!("  {} {}: {}", c.kind, c.name, c.destructive.join("; ")))
        .collect();
    if !flagged.is_empty() && !opts.allow_destructive {
        bail!(
            "destructive_change: the plan has {} destructive change{} and --allow-destructive is not set; nothing was changed:\n{}",
            flagged.len(),
            if flagged.len() == 1 { "" } else { "s" },
            flagged.join("\n")
        );
    }
    Ok(())
}

/// Refuses a `--secrets-out` file inside the project directory (project files never hold secrets).
pub fn check_secrets_out(project: &Project, opts: &Options) -> anyhow::Result<()> {
    let Some(p) = &opts.secrets_out else { return Ok(()) };
    let abs = |p: &Path| std::path::absolute(p).unwrap_or_else(|_| p.to_owned());
    let file = abs(p);
    let dir = abs(&project.dir);
    if file.starts_with(&dir) {
        bail!(
            "--secrets-out {} is inside the project directory {}; secrets are never written to project files",
            p.display(),
            project.dir.display()
        );
    }
    Ok(())
}

/// Runs the plan.
pub async fn execute(
    remote: &Remote,
    project: &Project,
    plan: &Plan,
    opts: &Options,
    out: &mut impl Write,
) -> anyhow::Result<Outcome> {
    check_destructive(plan, opts)?;
    check_secrets_out(project, opts)?;
    let mut ctx = Ctx {
        remote,
        project,
        opts,
        outcome: Outcome::default(),
        ids: remote
            .collection_names()
            .await?
            .into_iter()
            .map(|(id, n)| (n, id))
            .collect(),
        deferred: Vec::new(),
    };
    let mut failed: BTreeSet<String> = BTreeSet::new();
    for c in &plan.changes {
        if let Some(d) = c.depends_on.iter().find(|d| failed.contains(*d)) {
            writeln!(out, "  skipped {} {}: {d} failed", c.kind, c.name)?;
            ctx.outcome.skipped += 1;
            failed.insert(c.id());
            continue;
        }
        match ctx.run(c).await {
            Ok(Done::Applied(what)) => {
                writeln!(out, "  {what} {} {}", c.kind, c.name)?;
                ctx.outcome.applied += 1;
            }
            Ok(Done::InPlace) => {
                writeln!(out, "  already in place: {} {}", c.kind, c.name)?;
                ctx.outcome.in_place += 1;
            }
            Err(e) => {
                writeln!(out, "  FAILED {} {}: {e:#}", c.kind, c.name)?;
                ctx.outcome.failed.push((c.id(), format!("{e:#}")));
                failed.insert(c.id());
            }
        }
        out.flush()?;
    }
    // Relation fields whose target did not exist yet when their collection was created.
    for name in std::mem::take(&mut ctx.deferred) {
        let id = format!("collection:{name}");
        if failed.contains(&id) {
            continue;
        }
        if let Err(e) = ctx.update_collection(&name, &name).await {
            writeln!(out, "  FAILED collection {name} (relation fields): {e:#}")?;
            ctx.outcome.failed.push((id, format!("{e:#}")));
        } else {
            writeln!(out, "  added relation fields to collection {name}")?;
        }
    }
    let mut outcome = ctx.outcome;
    if !outcome.secrets.is_empty() {
        match &opts.secrets_out {
            Some(p) => write_secrets(p, &outcome.secrets)?,
            None => outcome.unsaved_secrets = outcome.secrets.keys().cloned().collect(),
        }
    }
    Ok(outcome)
}

/// Writes `NAME=value` lines (mode 0600 on Unix).
fn write_secrets(path: &Path, secrets: &BTreeMap<String, String>) -> anyhow::Result<()> {
    let mut text =
        String::from("# Secrets issued by `forgebase project apply`. Keep this file out of version control.\n");
    for (k, v) in secrets {
        text.push_str(&format!("{k}={v}\n"));
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .with_context(|| format!("cannot write {}", path.display()))?;
    f.write_all(text.as_bytes())?;
    Ok(())
}

enum Done {
    Applied(&'static str),
    InPlace,
}

struct Ctx<'a> {
    remote: &'a Remote,
    project: &'a Project,
    opts: &'a Options,
    outcome: Outcome,
    /// Collection name -> id.
    ids: HashMap<String, String>,
    /// Collections created without some relation fields.
    deferred: Vec<String>,
}

fn verb(a: Action) -> &'static str {
    match a {
        Action::Create => "created",
        Action::Update => "updated",
        Action::Replace => "replaced",
        Action::Delete => "deleted",
    }
}

fn tolerate_404(r: anyhow::Result<Value>) -> anyhow::Result<()> {
    match r {
        Ok(_) => Ok(()),
        Err(e) if is_not_found(&e) => Ok(()),
        Err(e) => Err(e),
    }
}

fn secret_var(kind: &str, name: &str) -> String {
    let up: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("FORGEBASE_{kind}_{up}_SECRET")
}

impl Ctx<'_> {
    fn desired(&self, c: &Change) -> anyhow::Result<&Desired> {
        self.project
            .get(c.kind, &c.name)
            .ok_or_else(|| anyhow!("{} {} is not in the project any more; plan again", c.kind, c.name))
    }

    /// The concurrency check: `None` to go ahead, `Some(InPlace)` when the
    /// server already has the project's state.
    async fn check(&self, c: &Change) -> anyhow::Result<Option<Done>> {
        let current = self.remote.fetch_one(c.kind, c.server_name()).await?;
        let now = current.as_ref().map(Actual::fingerprint);
        if now == c.fingerprint {
            return Ok(None);
        }
        // Already converged (a re-run of the same plan)?
        let in_place = match (c.action, &current) {
            (Action::Delete, None) => true,
            (Action::Delete, Some(_)) => false,
            (_, _) => {
                let target = if c.renamed_from.is_some() {
                    self.remote.fetch_one(c.kind, &c.name).await?
                } else {
                    current.clone()
                };
                match (self.project.get(c.kind, &c.name), target) {
                    (Some(d), Some(a)) => {
                        model::diff(&model::managed_view(c.kind, &d.spec, &a.spec), &d.spec).is_empty()
                    }
                    _ => false,
                }
            }
        };
        if in_place {
            return Ok(Some(Done::InPlace));
        }
        bail!(
            "changed_since_plan: {} {} changed on the server after the plan was made (by another apply, the dashboard or the API); run `forgebase project plan` again",
            c.kind,
            c.server_name()
        )
    }

    async fn run(&mut self, c: &Change) -> anyhow::Result<Done> {
        if let Some(done) = self.check(c).await? {
            return Ok(done);
        }
        if matches!(c.action, Action::Delete | Action::Replace) {
            self.delete(c.kind, c.server_name(), c.action == Action::Replace)
                .await?;
            if c.action == Action::Delete {
                return Ok(Done::Applied(verb(c.action)));
            }
        }
        let create = matches!(c.action, Action::Create | Action::Replace);
        match c.kind {
            Kind::Collection => {
                if create {
                    self.create_collection(c).await?;
                } else {
                    self.update_collection(c.server_name(), &c.name).await?;
                }
            }
            Kind::Bucket => self.bucket(c, create).await?,
            Kind::Topic => self.topic(c, create).await?,
            Kind::Subscription => self.subscription(c, create).await?,
            Kind::Function => self.function(c).await?,
            Kind::Policy => self.policy(c).await?,
            Kind::OauthClient => self.oauth_client(c, create).await?,
        }
        Ok(Done::Applied(verb(c.action)))
    }

    async fn delete(&self, kind: Kind, name: &str, replacing: bool) -> anyhow::Result<()> {
        let r = self.remote;
        let n = enc(name);
        let result = match kind {
            Kind::Collection => r.call("DELETE", &format!("/api/v1/collections/{n}"), None).await,
            Kind::Bucket => {
                r.call("DELETE", &format!("/api/v1/storage/buckets/{n}?force=true"), None)
                    .await
            }
            Kind::Topic => {
                let q = if replacing { "?force=true" } else { "" };
                r.call("DELETE", &format!("/api/v1/queues/topics/{n}{q}"), None).await
            }
            Kind::Subscription => {
                r.call("DELETE", &format!("/api/v1/queues/subscriptions/{n}"), None)
                    .await
            }
            Kind::Function => r.call("DELETE", &format!("/api/v1/functions/{n}"), None).await,
            Kind::Policy => {
                let (k, rn) = name
                    .split_once('/')
                    .ok_or_else(|| anyhow!("bad policy name {name:?}"))?;
                r.call(
                    "DELETE",
                    &format!("/api/v1/policies/resources/{}/{}", enc(k), enc(rn)),
                    None,
                )
                .await
            }
            Kind::OauthClient => {
                let Some(a) = r.fetch_one(Kind::OauthClient, name).await? else {
                    return Ok(());
                };
                let id = a.raw["client_id"].as_str().unwrap_or_default().to_owned();
                r.call("DELETE", &format!("/api/auth/clients/{}", enc(&id)), None).await
            }
        };
        tolerate_404(result)
    }

    // ----- collections ------------------------------------------------------

    /// Server form of the project's fields: relation names become ids
    /// (`None` when a target does not exist yet), field ids are kept for
    /// fields the server has (matched by name or `renamed_from`).
    fn server_fields(&self, d: &Desired, existing: Option<&Value>) -> (Vec<Value>, bool) {
        let renames = match &d.extra {
            Extra::Collection { field_renames, .. } => field_renames.clone(),
            _ => BTreeMap::new(),
        };
        let old_fields: Vec<Value> = existing
            .and_then(|e| e["fields"].as_array().cloned())
            .unwrap_or_default();
        let mut deferred = false;
        let mut out: Vec<Value> = old_fields
            .iter()
            .filter(|f| f["system"].as_bool().unwrap_or(false))
            .cloned()
            .collect();
        for f in d.spec["fields"].as_array().into_iter().flatten() {
            let mut f = f.clone();
            let name = f["name"].as_str().unwrap_or_default().to_owned();
            let o = f.as_object_mut().map(std::mem::take).unwrap_or_default();
            let mut m: Map<String, Value> = o;
            if let Some(target) = m.remove("collection") {
                let tname = target.as_str().unwrap_or_default();
                match self.ids.get(tname) {
                    Some(id) => {
                        m.insert("collection_id".into(), json!(id));
                    }
                    None if existing.is_none() => {
                        deferred = true;
                        continue;
                    }
                    None => {
                        m.insert("collection_id".into(), json!(tname));
                    }
                }
            }
            let lookup = |n: &str| {
                old_fields
                    .iter()
                    .find(|x| x["name"].as_str() == Some(n) && !x["system"].as_bool().unwrap_or(false))
            };
            let prior = lookup(&name).or_else(|| renames.get(&name).and_then(|old| lookup(old)));
            if let Some(id) = prior.and_then(|p| p["id"].as_str()) {
                m.insert("id".into(), json!(id));
            }
            out.push(Value::Object(m));
        }
        (out, deferred)
    }

    fn collection_body(&self, d: &Desired, existing: Option<&Value>) -> (Value, bool) {
        let (fields, deferred) = self.server_fields(d, existing);
        let mut options = existing
            .and_then(|e| e["options"].as_object().cloned())
            .unwrap_or_default();
        for (k, v) in d.spec["options"].as_object().into_iter().flatten() {
            options.insert(k.clone(), v.clone());
        }
        let mut body = json!({
            "name": d.name,
            "type": d.spec["type"],
            "schema_mode": d.spec["schema_mode"],
            "versioned": d.spec["versioned"],
            "options": options,
            "fields": fields,
        });
        if d.spec["type"] == "view" {
            body["view_query"] = d.spec["view_query"].clone();
            body["fields"] = json!([]);
        }
        (body, deferred)
    }

    async fn create_collection(&mut self, c: &Change) -> anyhow::Result<()> {
        let d = self.desired(c)?.clone();
        let (body, deferred) = self.collection_body(&d, None);
        let created = self.remote.call("POST", "/api/v1/collections", Some(&body)).await?;
        if let Some(id) = created["id"].as_str() {
            self.ids.insert(d.name.clone(), id.to_owned());
        }
        if deferred {
            self.deferred.push(d.name.clone());
        }
        self.sync_indexes(&d, &[]).await?;
        if let Some(rules) = d.spec.get("rules")
            && rules != &model::rules_spec(&json!({}))
        {
            self.put_rules(&d.name, rules).await?;
        }
        Ok(())
    }

    /// PUTs the project's definition of `name` (on the server as `server_name`), then indexes and rules.
    async fn update_collection(&mut self, server_name: &str, name: &str) -> anyhow::Result<()> {
        let d = self
            .project
            .get(Kind::Collection, name)
            .ok_or_else(|| anyhow!("collection {name} is not in the project"))?
            .clone();
        let existing = self
            .remote
            .call("GET", &format!("/api/v1/collections/{}", enc(server_name)), None)
            .await?;
        let before = self.remote.fetch_one(Kind::Collection, server_name).await?;
        let (body, _) = self.collection_body(&d, Some(&existing));
        let view_before = before
            .as_ref()
            .map(|a| model::managed_view(Kind::Collection, &d.spec, &a.spec));
        let definition_changed = view_before.as_ref().is_none_or(|v| {
            let strip = |x: &Value| {
                let mut x = x.clone();
                if let Some(o) = x.as_object_mut() {
                    o.remove("indexes");
                    o.remove("rules");
                }
                x
            };
            strip(v) != strip(&d.spec)
        }) || server_name != name;
        if definition_changed {
            self.remote
                .call("PUT", &format!("/api/v1/collections/{}", enc(server_name)), Some(&body))
                .await?;
        }
        let current_indexes: Vec<Value> = before
            .as_ref()
            .and_then(|a| a.spec["indexes"].as_array().cloned())
            .unwrap_or_default();
        self.sync_indexes(&d, &current_indexes).await?;
        if let Some(rules) = d.spec.get("rules")
            && before.as_ref().is_none_or(|a| a.spec.get("rules") != Some(rules))
        {
            self.put_rules(&d.name, rules).await?;
        }
        Ok(())
    }

    async fn sync_indexes(&self, d: &Desired, current: &[Value]) -> anyhow::Result<()> {
        let Some(wanted) = d.spec.get("indexes").and_then(Value::as_array) else {
            return Ok(());
        };
        for ix in current {
            if !wanted.contains(ix) {
                let n = ix["name"].as_str().unwrap_or_default();
                tolerate_404(
                    self.remote
                        .call("DELETE", &format!("/api/v1/indexes/{}", enc(n)), None)
                        .await,
                )?;
            }
        }
        for ix in wanted {
            if current.contains(ix) {
                continue;
            }
            let mut body = ix.clone();
            body["collection"] = json!(d.name);
            if body["partial"].is_null()
                && let Some(o) = body.as_object_mut()
            {
                o.remove("partial");
            }
            self.remote.call("POST", "/api/v1/indexes", Some(&body)).await?;
        }
        Ok(())
    }

    async fn put_rules(&self, collection: &str, rules: &Value) -> anyhow::Result<()> {
        self.remote
            .call("PUT", &format!("/api/v1/policies/{}", enc(collection)), Some(rules))
            .await
            .map(|_| ())
    }

    // ----- storage ----------------------------------------------------------

    async fn bucket(&self, c: &Change, create: bool) -> anyhow::Result<()> {
        let d = self.desired(c)?;
        let s = &d.spec;
        let n = enc(&d.name);
        let before = if create {
            None
        } else {
            self.remote.fetch_one(Kind::Bucket, &d.name).await?
        };
        if create {
            let mut body = json!({"name": d.name, "visibility": s["visibility"], "versioning": s["versioning"]});
            if !s["max_object_bytes"].is_null() {
                body["max_object_bytes"] = s["max_object_bytes"].clone();
            }
            if !s["allowed_content_types"].is_null() {
                body["allowed_content_types"] = s["allowed_content_types"].clone();
            }
            self.remote.call("POST", "/api/v1/storage/buckets", Some(&body)).await?;
        } else {
            let body = json!({
                "visibility": s["visibility"],
                "versioning": s["versioning"],
                "max_object_bytes": if s["max_object_bytes"].is_null() { json!(0) } else { s["max_object_bytes"].clone() },
                "allowed_content_types": if s["allowed_content_types"].is_null() { json!([]) } else { s["allowed_content_types"].clone() },
            });
            self.remote
                .call("PATCH", &format!("/api/v1/storage/buckets/{n}"), Some(&body))
                .await?;
        }
        if let Some(acl) = s.get("acl")
            && before.as_ref().is_none_or(|a| a.spec.get("acl") != Some(acl))
            && !(create && acl.as_array().is_some_and(Vec::is_empty))
        {
            self.remote
                .call(
                    "PUT",
                    &format!("/api/v1/storage/buckets/{n}/acl"),
                    Some(&json!({"entries": acl})),
                )
                .await?;
        }
        Ok(())
    }

    // ----- queues -----------------------------------------------------------

    async fn topic(&self, c: &Change, create: bool) -> anyhow::Result<()> {
        let d = self.desired(c)?;
        let s = &d.spec;
        let mut body = Map::new();
        body.insert("labels".into(), s["labels"].clone());
        for k in model::TOPIC_OPTIONAL {
            if let Some(v) = s.get(*k) {
                body.insert(
                    (*k).into(),
                    if v.is_null() && *k == "retention_bytes" && !create {
                        json!(0)
                    } else {
                        v.clone()
                    },
                );
            }
        }
        if create {
            body.insert("name".into(), json!(d.name));
            if !s["schema"].is_null() {
                body.insert("schema".into(), s["schema"].clone());
            }
            if !s["source"].is_null() {
                body.insert("source".into(), s["source"].clone());
            }
            body.retain(|_, v| !v.is_null());
            self.remote
                .call("POST", "/api/v1/queues/topics", Some(&Value::Object(body)))
                .await?;
        } else {
            if s["schema"].is_null() {
                body.insert("clear_schema".into(), json!(true));
            } else {
                body.insert("schema".into(), s["schema"].clone());
            }
            self.remote
                .call(
                    "PATCH",
                    &format!("/api/v1/queues/topics/{}", enc(&d.name)),
                    Some(&Value::Object(body)),
                )
                .await?;
        }
        Ok(())
    }

    fn keep_secret(&mut self, var: String, secret: Option<&str>) {
        if let Some(s) = secret.filter(|s| !s.is_empty()) {
            self.outcome.secrets.insert(var, s.to_owned());
        }
    }

    async fn subscription(&mut self, c: &Change, create: bool) -> anyhow::Result<()> {
        let d = self.desired(c)?.clone();
        let s = &d.spec;
        let (start, secret_env) = match &d.extra {
            Extra::Subscription { start, secret_env } => (start.clone(), secret_env.clone()),
            _ => (None, None),
        };
        let var = secret_env.unwrap_or_else(|| secret_var("SUBSCRIPTION", &d.name));
        let mut body = Map::new();
        body.insert("labels".into(), s["labels"].clone());
        for k in model::SUBSCRIPTION_OPTIONAL {
            if let Some(v) = s.get(*k) {
                body.insert((*k).into(), v.clone());
            }
        }
        let answer = if create {
            body.insert("name".into(), json!(d.name));
            body.insert("topic".into(), s["topic"].clone());
            body.insert("ordering".into(), s["ordering"].clone());
            for k in ["filter", "push", "dead_letter_topic", "max_delivery_attempts"] {
                if !s[k].is_null() {
                    body.insert(k.into(), s[k].clone());
                }
            }
            if let Some(st) = start {
                body.insert("start".into(), st);
            }
            body.retain(|_, v| !v.is_null());
            self.remote
                .call("POST", "/api/v1/queues/subscriptions", Some(&Value::Object(body)))
                .await?
        } else {
            let before = self.remote.fetch_one(Kind::Subscription, &d.name).await?;
            let old = before.map(|a| a.spec).unwrap_or(Value::Null);
            if old["push"] != s["push"] {
                if s["push"].is_null() {
                    body.insert("clear_push".into(), json!(true));
                } else {
                    body.insert("push".into(), s["push"].clone());
                }
            }
            if s["dead_letter_topic"].is_null() {
                if !old["dead_letter_topic"].is_null() {
                    body.insert("clear_dead_letter_topic".into(), json!(true));
                }
            } else {
                body.insert("dead_letter_topic".into(), s["dead_letter_topic"].clone());
            }
            body.insert(
                "max_delivery_attempts".into(),
                if s["max_delivery_attempts"].is_null() {
                    json!(0)
                } else {
                    s["max_delivery_attempts"].clone()
                },
            );
            body.retain(|_, v| !v.is_null());
            self.remote
                .call(
                    "PATCH",
                    &format!("/api/v1/queues/subscriptions/{}", enc(&d.name)),
                    Some(&Value::Object(body)),
                )
                .await?
        };
        self.keep_secret(var, answer["push_secret"].as_str());
        Ok(())
    }

    // ----- functions --------------------------------------------------------

    async fn function(&self, c: &Change) -> anyhow::Result<()> {
        let d = self.desired(c)?;
        let Extra::Function(fx) = &d.extra else {
            bail!("function {} has no code", d.name);
        };
        let code = fx.code.as_ref().ok_or_else(|| {
            anyhow!(
                "{} does not exist yet: run `forgebase deploy`, which runs the function's build command first",
                fx.entry.display()
            )
        })?;
        let s = &d.spec;
        let path = format!("/api/v1/functions/{}", enc(&d.name));
        let before = self.remote.fetch_one(Kind::Function, &d.name).await?;
        let strip = |v: &Value| {
            let mut v = v.clone();
            if let Some(o) = v.as_object_mut() {
                o.remove("secrets");
                o.remove("kv_rules");
            }
            v
        };
        let view = before.as_ref().map(|a| model::managed_view(Kind::Function, s, &a.spec));
        if view.as_ref().is_none_or(|v| strip(v) != strip(s)) {
            let mut body = json!({
                "name": d.name,
                "description": s["description"],
                "access": s["access"],
                "capabilities": s["capabilities"],
                "fetch_hosts": s["fetch_hosts"],
                "env": s["env"],
                "hooks": s["hooks"],
                "limits": s["limits"],
                "activate": true,
            });
            match code {
                Code::Wasm(b) => body["wasm"] = json!(base64::engine::general_purpose::STANDARD.encode(b)),
                Code::Source { language, text } => {
                    body["source"] = json!(text);
                    body["language"] = json!(language);
                }
            }
            self.remote.call("POST", "/api/v1/functions", Some(&body)).await?;
        }
        // Secrets: values come from the environment, never from files.
        let have: BTreeSet<String> = before
            .as_ref()
            .and_then(|a| a.spec["secrets"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        for (key, var) in &fx.secrets {
            if have.contains(key) && !self.opts.update_secrets {
                continue;
            }
            let value = std::env::var(var).map_err(|_| {
                anyhow!(
                    "secret {key} of function {}: environment variable {var} is not set",
                    d.name
                )
            })?;
            self.remote
                .call(
                    "PUT",
                    &format!("{path}/secrets/{}", enc(key)),
                    Some(&json!({"value": value})),
                )
                .await?;
        }
        for key in have.iter().filter(|k| !fx.secrets.contains_key(*k)) {
            tolerate_404(
                self.remote
                    .call("DELETE", &format!("{path}/secrets/{}", enc(key)), None)
                    .await,
            )?;
        }
        if let Some(rules) = s.get("kv_rules")
            && before.as_ref().is_none_or(|a| a.spec.get("kv_rules") != Some(rules))
        {
            self.remote
                .call("PUT", &format!("{path}/kv-rules"), Some(&json!({"items": rules})))
                .await?;
        }
        Ok(())
    }

    // ----- policies and clients ---------------------------------------------

    async fn policy(&self, c: &Change) -> anyhow::Result<()> {
        let d = self.desired(c)?;
        let (k, n) = d
            .name
            .split_once('/')
            .ok_or_else(|| anyhow!("bad policy name {}", d.name))?;
        self.remote
            .call(
                "PUT",
                &format!("/api/v1/policies/resources/{}/{}", enc(k), enc(n)),
                Some(&json!({"rules": d.spec["rules"]})),
            )
            .await
            .map(|_| ())
    }

    async fn oauth_client(&mut self, c: &Change, create: bool) -> anyhow::Result<()> {
        let d = self.desired(c)?.clone();
        let s = &d.spec;
        let mut body = json!({
            "name": d.name,
            "description": s["description"],
            "redirect_uris": s["redirect_uris"],
            "post_logout_redirect_uris": s["post_logout_redirect_uris"],
            "skip_consent": s["skip_consent"],
            "access_token_ttl": s["access_token_ttl"],
            "refresh_token_ttl": s["refresh_token_ttl"],
        });
        for k in model::OAUTH_OPTIONAL {
            if let Some(v) = s.get(*k) {
                body[*k] = v.clone();
            }
        }
        if let Some(o) = body.as_object_mut() {
            o.retain(|_, v| !v.is_null());
        }
        if create {
            body["realm"] = json!(self.remote.realm);
            body["type"] = s["type"].clone();
            let created = self.remote.call("POST", "/api/auth/clients", Some(&body)).await?;
            let var = match &d.extra {
                Extra::OauthClient { secret_env: Some(v) } => v.clone(),
                _ => secret_var("OAUTH_CLIENT", &d.name),
            };
            self.keep_secret(var, created["client_secret"].as_str());
            if s["disabled"] == true {
                let id = created["client"]["client_id"].as_str().unwrap_or_default();
                self.remote
                    .call(
                        "PATCH",
                        &format!("/api/auth/clients/{}", enc(id)),
                        Some(&json!({"disabled": true})),
                    )
                    .await?;
            }
        } else {
            let a = self
                .remote
                .fetch_one(Kind::OauthClient, &d.name)
                .await?
                .ok_or_else(|| anyhow!("OAuth client {} disappeared", d.name))?;
            body["disabled"] = s["disabled"].clone();
            let id = a.raw["client_id"].as_str().unwrap_or_default().to_owned();
            self.remote
                .call("PATCH", &format!("/api/auth/clients/{}", enc(&id)), Some(&body))
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::plan::Summary;

    fn plan_with(destructive: bool) -> Plan {
        Plan {
            format: 1,
            server: "http://x".into(),
            project: "p".into(),
            prune: true,
            changes: vec![Change {
                kind: Kind::Bucket,
                name: "b".into(),
                action: Action::Delete,
                renamed_from: None,
                file: None,
                destructive: if destructive {
                    vec!["deletes objects".into()]
                } else {
                    vec![]
                },
                migration: vec![],
                notes: vec![],
                diff: vec![],
                fingerprint: None,
                desired_digest: None,
                depends_on: vec![],
            }],
            unmanaged: vec![],
            summary: Summary::default(),
        }
    }

    #[test]
    fn destructive_changes_need_the_flag() {
        let opts = Options::default();
        let err = check_destructive(&plan_with(true), &opts).unwrap_err().to_string();
        assert!(
            err.starts_with("destructive_change:") && err.contains("bucket b: deletes objects"),
            "{err}"
        );
        assert!(check_destructive(&plan_with(false), &opts).is_ok());
        let allow = Options {
            allow_destructive: true,
            ..Options::default()
        };
        assert!(check_destructive(&plan_with(true), &allow).is_ok());
    }

    #[test]
    fn secret_variables_and_files() {
        assert_eq!(
            secret_var("SUBSCRIPTION", "orders.hook-1"),
            "FORGEBASE_SUBSCRIPTION_ORDERS_HOOK_1_SECRET"
        );
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("s.env");
        write_secrets(&p, &[("A".to_owned(), "x".to_owned())].into()).unwrap();
        assert!(std::fs::read_to_string(&p).unwrap().ends_with("A=x\n"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }
}
