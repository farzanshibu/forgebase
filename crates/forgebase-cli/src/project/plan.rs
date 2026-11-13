//! The plan: what `apply` would change, per resource, with the reasons a
//! change is destructive or rewrites data (ADR-0037 §4).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;

use anyhow::bail;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::layout::Project;
use super::model::{self, Actual, Delta, Desired, Extra, Key, Kind};

/// Format version of plan files and `--json` output.
pub const PLAN_FORMAT: i64 = 1;

/// What happens to a resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// New on the server.
    Create,
    /// Changed in place.
    Update,
    /// Deleted and created again (a setting that cannot change in place changed).
    Replace,
    /// Removed from the server (only with `--prune`).
    Delete,
}

impl Action {
    fn symbol(self) -> &'static str {
        match self {
            Action::Create => "+",
            Action::Update => "~",
            Action::Replace => "-/+",
            Action::Delete => "-",
        }
    }
}

/// One planned change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    /// Kind.
    pub kind: Kind,
    /// Name in the project (the new name of a rename).
    pub name: String,
    /// What happens.
    pub action: Action,
    /// Previous name on the server (a rename).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renamed_from: Option<String>,
    /// File that declares it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Why the change loses data; non-empty means `--allow-destructive` is needed.
    #[serde(default)]
    pub destructive: Vec<String>,
    /// Data the server rewrites or checks (table rebuild, unique index over existing rows).
    #[serde(default)]
    pub migration: Vec<String>,
    /// Other things worth knowing.
    #[serde(default)]
    pub notes: Vec<String>,
    /// Differences (server -> project).
    #[serde(default)]
    pub diff: Vec<Delta>,
    /// Fingerprint of the server state the plan saw (`None`: absent).
    #[serde(default)]
    pub fingerprint: Option<String>,
    /// Digest of the project's spec (`None`: a delete).
    #[serde(default)]
    pub desired_digest: Option<String>,
    /// Resources this change needs first (`kind:name`).
    #[serde(default)]
    pub depends_on: Vec<String>,
}

impl Change {
    /// The server-side name the change starts from.
    pub fn server_name(&self) -> &str {
        self.renamed_from.as_deref().unwrap_or(&self.name)
    }

    /// `kind:name`.
    pub fn id(&self) -> String {
        format!("{}:{}", self.kind, self.name)
    }
}

/// A server resource the project does not declare.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unmanaged {
    /// Kind.
    pub kind: Kind,
    /// Name.
    pub name: String,
}

/// A whole plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    /// Format version.
    pub format: i64,
    /// Server base URL.
    pub server: String,
    /// Project name.
    pub project: String,
    /// Whether deletions were planned (`--prune`).
    pub prune: bool,
    /// Changes in apply order.
    pub changes: Vec<Change>,
    /// Server resources the project does not declare (deleted only with `--prune`).
    pub unmanaged: Vec<Unmanaged>,
    /// Counts.
    pub summary: Summary,
}

/// Change counts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    /// Creates.
    pub create: usize,
    /// Updates.
    pub update: usize,
    /// Replacements.
    pub replace: usize,
    /// Deletes.
    pub delete: usize,
    /// Changes that need `--allow-destructive`.
    pub destructive: usize,
    /// Server resources the project does not declare.
    pub unmanaged: usize,
}

impl Plan {
    /// Whether applying would change anything.
    pub fn has_changes(&self) -> bool {
        !self.changes.is_empty()
    }
}

/// Planning inputs besides the project and the server state.
#[derive(Debug, Clone, Default)]
pub struct Inputs {
    /// Server base URL.
    pub server: String,
    /// Plan deletions.
    pub prune: bool,
    /// Names of every server collection (system ones included).
    pub server_collections: BTreeSet<String>,
}

fn dep(kind: Kind, name: &str) -> String {
    format!("{kind}:{name}")
}

fn policy_target(name: &str) -> Option<(Kind, &str)> {
    let (k, n) = name.split_once('/')?;
    let kind = match k {
        "bucket" => Kind::Bucket,
        "queue_topic" => Kind::Topic,
        "queue_subscription" => Kind::Subscription,
        _ => return None,
    };
    (n != "*").then_some((kind, n))
}

/// Resources a desired resource needs on the server first.
fn dependencies(d: &Desired) -> Vec<(Kind, String)> {
    let mut out = Vec::new();
    let s = &d.spec;
    match d.kind {
        Kind::Collection => {
            for f in s["fields"].as_array().into_iter().flatten() {
                if let Some(c) = f["collection"].as_str()
                    && f["type"] == "relation"
                    && c != d.name
                {
                    out.push((Kind::Collection, c.to_owned()));
                }
            }
        }
        Kind::Topic => {
            if let Some(c) = s["source"]["collection"].as_str() {
                out.push((Kind::Collection, c.to_owned()));
            }
        }
        Kind::Function => {
            for h in s["hooks"].as_array().into_iter().flatten() {
                if let Some(c) = h["collection"].as_str() {
                    out.push((Kind::Collection, c.to_owned()));
                }
            }
        }
        Kind::Subscription => {
            if let Some(t) = s["topic"].as_str() {
                out.push((Kind::Topic, t.to_owned()));
            }
            if let Some(t) = s["dead_letter_topic"].as_str() {
                out.push((Kind::Topic, t.to_owned()));
            }
            if s["push"]["kind"] == "function"
                && let Some(f) = s["push"]["endpoint"].as_str()
            {
                out.push((Kind::Function, f.to_owned()));
            }
        }
        Kind::Policy => {
            if let Some((k, n)) = policy_target(&d.name) {
                out.push((k, n.to_owned()));
            }
        }
        Kind::Bucket | Kind::OauthClient => {}
    }
    out.sort();
    out.dedup();
    out
}

/// Builds the plan for `project` against the server's `actual` state.
pub fn compute(project: &Project, actual: &BTreeMap<Key, Actual>, inputs: &Inputs) -> anyhow::Result<Plan> {
    let managed: Vec<&Desired> = project
        .resources
        .iter()
        .filter(|r| project.config.manages(r.kind))
        .collect();
    let declared: BTreeSet<Key> = managed.iter().map(|d| (d.kind, d.name.clone())).collect();

    // References must resolve to the project or the server.
    let mut problems = Vec::new();
    for d in &managed {
        for (kind, name) in dependencies(d) {
            let known = declared.contains(&(kind, name.clone()))
                || actual.contains_key(&(kind, name.clone()))
                || (kind == Kind::Collection && inputs.server_collections.contains(&name))
                || !project.config.manages(kind);
            if !known {
                problems.push(format!(
                    "{}: {} {:?} references {kind} {name:?}, which is neither in the project nor on the server",
                    project.dir.join(&d.file).display(),
                    d.kind,
                    d.name
                ));
            }
        }
    }
    if !problems.is_empty() {
        bail!("{}", problems.join("\n"));
    }

    let mut changes: Vec<Change> = Vec::new();
    let mut consumed: BTreeSet<Key> = BTreeSet::new();
    let mut replaced_topics: BTreeSet<String> = BTreeSet::new();
    for d in &managed {
        let mut renamed_from = None;
        let mut current = actual.get(&(d.kind, d.name.clone()));
        if current.is_none()
            && let Extra::Collection {
                renamed_from: Some(old),
                ..
            } = &d.extra
            && !declared.contains(&(Kind::Collection, old.clone()))
            && let Some(a) = actual.get(&(Kind::Collection, old.clone()))
        {
            current = Some(a);
            renamed_from = Some(old.clone());
        }
        let deps: Vec<String> = dependencies(d).iter().map(|(k, n)| dep(*k, n)).collect();
        let file = Some(project.dir.join(&d.file).display().to_string());
        let Some(a) = current else {
            changes.push(Change {
                kind: d.kind,
                name: d.name.clone(),
                action: Action::Create,
                renamed_from: None,
                file,
                destructive: Vec::new(),
                migration: Vec::new(),
                notes: create_notes(d),
                diff: Vec::new(),
                fingerprint: None,
                desired_digest: Some(model::digest(&d.spec)),
                depends_on: deps,
            });
            continue;
        };
        consumed.insert((a.kind, a.name.clone()));
        let mut view = model::managed_view(d.kind, &d.spec, &a.spec);
        let mut notes = Vec::new();
        if let Some(old) = &renamed_from {
            notes.push(format!("renames collection {old:?} to {:?} (records are kept)", d.name));
        }
        if let Extra::Collection { field_renames, .. } = &d.extra {
            apply_field_renames(&mut view, field_renames, &mut notes);
        }
        let diff = model::diff(&view, &d.spec);
        if diff.is_empty() && renamed_from.is_none() {
            continue;
        }
        let mut c = Change {
            kind: d.kind,
            name: d.name.clone(),
            action: Action::Update,
            renamed_from,
            file,
            destructive: Vec::new(),
            migration: Vec::new(),
            notes,
            diff,
            fingerprint: Some(a.fingerprint()),
            desired_digest: Some(model::digest(&d.spec)),
            depends_on: deps,
        };
        classify(&mut c, &view, &d.spec, a);
        if c.kind == Kind::Topic && c.action == Action::Replace {
            replaced_topics.insert(c.name.clone());
        }
        changes.push(c);
    }
    // Subscriptions of a replaced topic are deleted with it: create them again.
    for c in changes.iter_mut().filter(|c| c.kind == Kind::Subscription) {
        if let Some(d) = project.get(Kind::Subscription, &c.name)
            && d.spec["topic"].as_str().is_some_and(|t| replaced_topics.contains(t))
            && c.action == Action::Update
        {
            c.action = Action::Replace;
            c.destructive
                .push("its topic is replaced, which deletes the subscription".into());
        }
    }
    for d in managed.iter().filter(|d| d.kind == Kind::Subscription) {
        let on_replaced = d.spec["topic"].as_str().is_some_and(|t| replaced_topics.contains(t));
        if on_replaced
            && actual.contains_key(&(Kind::Subscription, d.name.clone()))
            && !changes.iter().any(|c| c.kind == Kind::Subscription && c.name == d.name)
        {
            changes.push(Change {
                kind: Kind::Subscription,
                name: d.name.clone(),
                action: Action::Replace,
                renamed_from: None,
                file: Some(project.dir.join(&d.file).display().to_string()),
                destructive: vec!["its topic is replaced, which deletes the subscription".into()],
                migration: Vec::new(),
                notes: Vec::new(),
                diff: Vec::new(),
                fingerprint: actual
                    .get(&(Kind::Subscription, d.name.clone()))
                    .map(Actual::fingerprint),
                desired_digest: Some(model::digest(&d.spec)),
                depends_on: dependencies(d).iter().map(|(k, n)| dep(*k, n)).collect(),
            });
        }
    }

    let mut unmanaged = Vec::new();
    for ((kind, name), a) in actual {
        if !project.config.manages(*kind)
            || declared.contains(&(*kind, name.clone()))
            || consumed.contains(&(*kind, name.clone()))
            || model::is_builtin(*kind, name)
        {
            continue;
        }
        if !inputs.prune {
            unmanaged.push(Unmanaged {
                kind: *kind,
                name: name.clone(),
            });
            continue;
        }
        let destructive = match kind {
            Kind::Collection => vec![format!("drops collection {name:?} and all its records")],
            Kind::Bucket => vec![format!("deletes bucket {name:?} and all its objects")],
            Kind::Topic => vec![format!("deletes topic {name:?} and its retained messages")],
            Kind::Subscription => vec![format!("deletes subscription {name:?} and its undelivered messages")],
            Kind::Function => vec![format!("deletes function {name:?}, its versions, secrets and logs")],
            Kind::OauthClient => vec![format!("deletes OAuth client {name:?}: its tokens stop working")],
            Kind::Policy => Vec::new(),
        };
        let notes = if *kind == Kind::Policy {
            vec!["the resource falls back to the `*` rules, or is locked".into()]
        } else {
            Vec::new()
        };
        changes.push(Change {
            kind: *kind,
            name: name.clone(),
            action: Action::Delete,
            renamed_from: None,
            file: None,
            destructive,
            migration: Vec::new(),
            notes,
            diff: Vec::new(),
            fingerprint: Some(a.fingerprint()),
            desired_digest: None,
            depends_on: Vec::new(),
        });
    }

    order(&mut changes, actual);
    let mut summary = Summary {
        unmanaged: unmanaged.len(),
        ..Summary::default()
    };
    for c in &changes {
        match c.action {
            Action::Create => summary.create += 1,
            Action::Update => summary.update += 1,
            Action::Replace => summary.replace += 1,
            Action::Delete => summary.delete += 1,
        }
        if !c.destructive.is_empty() {
            summary.destructive += 1;
        }
    }
    Ok(Plan {
        format: PLAN_FORMAT,
        server: inputs.server.clone(),
        project: project.config.name.clone(),
        prune: inputs.prune,
        changes,
        unmanaged,
        summary,
    })
}

fn create_notes(d: &Desired) -> Vec<String> {
    match (&d.kind, &d.extra) {
        (Kind::Subscription, _) if d.spec["push"]["kind"] == "webhook" => {
            vec!["the server issues a webhook signing secret (shown once; see --secrets-out)".into()]
        }
        (Kind::OauthClient, _) if d.spec["type"] != "public" => {
            vec!["the server issues a client secret (shown once; see --secrets-out)".into()]
        }
        (Kind::Function, Extra::Function(f)) if f.code.is_none() => {
            vec!["the entry is built by `forgebase deploy` (build command)".into()]
        }
        _ => Vec::new(),
    }
}

/// Renames fields of the server view to their new names, so a rename diffs as a rename.
fn apply_field_renames(view: &mut Value, renames: &BTreeMap<String, String>, notes: &mut Vec<String>) {
    let Some(fields) = view.get_mut("fields").and_then(Value::as_array_mut) else {
        return;
    };
    for (new, old) in renames {
        let has_new = fields.iter().any(|f| f["name"].as_str() == Some(new.as_str()));
        if has_new {
            continue;
        }
        if let Some(f) = fields.iter_mut().find(|f| f["name"].as_str() == Some(old.as_str())) {
            f["name"] = json!(new);
            notes.push(format!("renames field {old:?} to {new:?} (its data is kept)"));
        }
    }
}

fn fields_by_name(spec: &Value) -> HashMap<String, Value> {
    spec["fields"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| Some((f["name"].as_str()?.to_owned(), f.clone())))
        .collect()
}

/// Storage-relevant shape of a field (a change rebuilds the table).
fn storage_shape(f: &Value) -> Value {
    let multi = f["max_select"].as_i64().unwrap_or(1) > 1;
    json!({
        "type": f["type"],
        "multi": multi,
        "integer": f["integer"],
        "collection": f["collection"],
        "cascade": f["cascade_delete"],
    })
}

/// Sets the action and the destructive / migration reasons of an update.
fn classify(c: &mut Change, before: &Value, after: &Value, a: &Actual) {
    let changed = |k: &str| before.get(k) != after.get(k);
    match c.kind {
        Kind::Collection => {
            let tenant = |v: &Value| v["options"]["tenant_scoped"].as_bool().unwrap_or(false);
            if changed("type") {
                c.action = Action::Replace;
                c.destructive.push(format!(
                    "the collection type cannot change ({} -> {}): the collection is dropped with its records and created again",
                    before["type"], after["type"]
                ));
                return;
            }
            if after["options"].get("tenant_scoped").is_some() && tenant(before) != tenant(after) {
                c.action = Action::Replace;
                c.destructive.push(
                    "tenant_scoped is fixed at creation: the collection is dropped with its records and created again"
                        .into(),
                );
                return;
            }
            let old = fields_by_name(before);
            let new = fields_by_name(after);
            let mut rebuild = false;
            for (name, f) in &old {
                match new.get(name) {
                    None => {
                        c.destructive.push(format!("drops field {name:?} and its data"));
                        rebuild = true;
                    }
                    Some(n) if n["type"] != f["type"] => {
                        c.destructive.push(format!(
                            "changes the type of field {name:?} ({} -> {}): values are converted, and the apply fails and rolls back if one does not convert",
                            f["type"], n["type"]
                        ));
                        rebuild = true;
                    }
                    Some(n) => {
                        let (fm, nm) = (
                            f["max_select"].as_i64().unwrap_or(1),
                            n["max_select"].as_i64().unwrap_or(1),
                        );
                        if fm > 1 && nm == 1 {
                            c.destructive.push(format!(
                                "field {name:?} goes from {fm} values to one: records holding several values fail the conversion"
                            ));
                        }
                        if storage_shape(f) != storage_shape(n) {
                            rebuild = true;
                        }
                        if n["unique"] == true && f["unique"] != true {
                            c.migration.push(format!(
                                "field {name:?} becomes unique: a unique index is built over the existing records (fails on duplicates)"
                            ));
                        }
                        if n["required"] == true && f["required"] != true {
                            c.notes.push(format!(
                                "field {name:?} becomes required: existing records are not checked, later writes must set it"
                            ));
                        }
                        if f["type"] == "select" {
                            let gone: Vec<&Value> = f["values"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter(|v| !n["values"].as_array().is_some_and(|a| a.contains(v)))
                                .collect();
                            if !gone.is_empty() {
                                c.notes.push(format!(
                                    "field {name:?} no longer allows {}: existing records keep those values",
                                    gone.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", ")
                                ));
                            }
                        }
                    }
                }
            }
            if rebuild {
                c.migration.push(
                    "the table is rebuilt in one transaction (existing records are copied and checked against the new definition)".into(),
                );
            }
            if changed("fields")
                || changed("type")
                || changed("schema_mode")
                || changed("versioned")
                || changed("options")
                || changed("view_query")
                || c.renamed_from.is_some()
            {
                c.notes.push(format!(
                    "schema version v{} -> v{} (recorded in GET /api/v1/collections/{}/history)",
                    a.raw["version"].as_i64().unwrap_or(0),
                    a.raw["version"].as_i64().unwrap_or(0) + 1,
                    a.name
                ));
            }
            if changed("indexes") {
                c.notes.push("indexes are dropped and created as listed".into());
            }
        }
        Kind::Topic => {
            if changed("source") {
                c.action = Action::Replace;
                c.destructive.push(
                    "a record source cannot change: the topic is deleted with its messages and subscriptions and created again".into(),
                );
            }
        }
        Kind::Subscription => {
            for k in ["topic", "filter", "ordering"] {
                if changed(k) {
                    c.action = Action::Replace;
                    c.destructive.push(format!(
                        "{k} cannot change: the subscription is deleted with its undelivered messages and created again"
                    ));
                }
            }
            if c.action == Action::Update && changed("push") && after["push"]["kind"] == "webhook" {
                c.notes
                    .push("a new webhook endpoint gets a new signing secret (see --secrets-out)".into());
            }
        }
        Kind::OauthClient => {
            if changed("type") {
                c.action = Action::Replace;
                c.destructive.push(
                    "the client type cannot change: the client is deleted (its tokens stop working) and created with a new client_id".into(),
                );
            }
        }
        Kind::Function => {
            let mut code_or_config = before.clone();
            let mut wanted = after.clone();
            for v in [&mut code_or_config, &mut wanted] {
                if let Some(o) = v.as_object_mut() {
                    o.remove("secrets");
                    o.remove("kv_rules");
                }
            }
            if code_or_config != wanted {
                c.notes.push(format!(
                    "deploys a new version (now v{})",
                    a.raw["active_version"].as_i64().unwrap_or(0)
                ));
            }
        }
        Kind::Bucket | Kind::Policy => {}
    }
}

/// Apply order: kinds in [`Kind::ALL`] order, collections and topics by
/// dependency; deletes last, in reverse.
fn order(changes: &mut Vec<Change>, actual: &BTreeMap<Key, Actual>) {
    let (mut deletes, mut rest): (Vec<Change>, Vec<Change>) =
        changes.drain(..).partition(|c| c.action == Action::Delete);
    rest.sort_by(|a, b| (a.kind, &a.name).cmp(&(b.kind, &b.name)));
    let mut sorted: Vec<Change> = Vec::with_capacity(rest.len());
    for kind in Kind::ALL {
        let (group, others): (Vec<Change>, Vec<Change>) = rest.into_iter().partition(|c| c.kind == kind);
        rest = others;
        sorted.extend(topological(group));
    }
    // Deletes: reverse kind order; a collection others point to goes last.
    deletes.sort_by(|a, b| {
        let referenced = |c: &Change| {
            c.kind == Kind::Collection
                && actual.values().any(|x| {
                    x.kind == Kind::Collection
                        && x.name != c.name
                        && x.spec["fields"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .any(|f| f["collection"].as_str() == Some(c.name.as_str()))
                })
        };
        (std::cmp::Reverse(a.kind), referenced(a), &a.name).cmp(&(std::cmp::Reverse(b.kind), referenced(b), &b.name))
    });
    sorted.extend(deletes);
    *changes = sorted;
}

/// Changes whose dependencies inside the group come first (a cycle keeps name order).
fn topological(mut pending: Vec<Change>) -> Vec<Change> {
    let mut out = Vec::with_capacity(pending.len());
    while !pending.is_empty() {
        let ids: BTreeSet<String> = pending.iter().map(Change::id).collect();
        let i = pending
            .iter()
            .position(|c| !c.depends_on.iter().any(|d| *d != c.id() && ids.contains(d)))
            .unwrap_or(0);
        out.push(pending.remove(i));
    }
    out
}

fn value_text(v: &Option<Value>) -> String {
    match v {
        None => "(absent)".into(),
        Some(v) => {
            let t = model::canonical_text(v);
            if t.chars().count() > 100 {
                format!("{}…", t.chars().take(100).collect::<String>())
            } else {
                t
            }
        }
    }
}

/// Human-readable plan.
pub fn render(plan: &Plan, out: &mut impl Write) -> std::io::Result<()> {
    writeln!(out, "Plan for {} against {}", plan.project, plan.server)?;
    writeln!(out)?;
    if plan.changes.is_empty() {
        writeln!(out, "No changes: the server matches the project.")?;
    }
    for c in &plan.changes {
        let mut head = format!("{} {} {}", c.action.symbol(), c.kind, c.name);
        if let Some(old) = &c.renamed_from {
            head.push_str(&format!(" (was {old})"));
        }
        let tag = match c.action {
            Action::Create => "create",
            Action::Update => "update",
            Action::Replace => "replace",
            Action::Delete => "delete",
        };
        head.push_str(&format!(
            "  [{tag}{}]",
            if c.destructive.is_empty() { "" } else { ", destructive" }
        ));
        writeln!(out, "{head}")?;
        for d in &c.diff {
            writeln!(
                out,
                "    {}: {} -> {}",
                d.path,
                value_text(&d.before),
                value_text(&d.after)
            )?;
        }
        for r in &c.destructive {
            writeln!(out, "    ! destructive: {r}")?;
        }
        for m in &c.migration {
            writeln!(out, "    * data migration: {m}")?;
        }
        for n in &c.notes {
            writeln!(out, "    note: {n}")?;
        }
    }
    let s = &plan.summary;
    writeln!(out)?;
    writeln!(
        out,
        "Plan: {} to create, {} to update, {} to replace, {} to delete{}.",
        s.create,
        s.update,
        s.replace,
        s.delete,
        if s.destructive > 0 {
            format!(" ({} destructive: needs --allow-destructive)", s.destructive)
        } else {
            String::new()
        }
    )?;
    if !plan.unmanaged.is_empty() {
        let list: Vec<String> = plan
            .unmanaged
            .iter()
            .map(|u| format!("{} {}", u.kind, u.name))
            .collect();
        writeln!(
            out,
            "Not in the project (kept; `--prune` deletes them): {}",
            list.join(", ")
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::layout::{ProjectConfig, Target};
    use crate::project::model::Area;
    use std::path::PathBuf;

    fn project(resources: Vec<Desired>) -> Project {
        Project {
            dir: PathBuf::from("forgebase"),
            config: ProjectConfig {
                name: "t".into(),
                manage: Area::ALL.into_iter().collect(),
                defaults: Target::default(),
                environments: BTreeMap::new(),
            },
            resources,
            seeds: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn coll(name: &str, file: Value) -> Desired {
        Desired {
            kind: Kind::Collection,
            name: name.into(),
            spec: model::collection_spec(&file),
            file: PathBuf::from(format!("collections/{name}.json")),
            extra: Extra::Collection {
                renamed_from: file["renamed_from"].as_str().map(str::to_owned),
                field_renames: file["fields"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|f| Some((f["name"].as_str()?.to_owned(), f["renamed_from"].as_str()?.to_owned())))
                    .collect(),
            },
        }
    }

    fn server(name: &str, fields: Value, version: i64) -> (Key, Actual) {
        let raw = json!({"id": format!("id_{name}"), "name": name, "type": "base", "schema_mode": "strict", "versioned": false, "fields": fields, "options": {}, "version": version});
        let spec = model::collection_spec_from_server(&raw, &HashMap::new(), &[], Some(&json!({})));
        (
            (Kind::Collection, name.into()),
            Actual {
                kind: Kind::Collection,
                name: name.into(),
                spec,
                revision: format!("v{version}"),
                raw,
            },
        )
    }

    fn inputs(prune: bool) -> Inputs {
        Inputs {
            server: "http://x".into(),
            prune,
            server_collections: BTreeSet::new(),
        }
    }

    #[test]
    fn creates_updates_and_flags_destructive_changes() {
        let p = project(vec![
            coll(
                "posts",
                json!({"fields": [{"name": "title", "type": "number"}], "rules": {}}),
            ),
            coll("tags", json!({"fields": [{"name": "label", "type": "text"}]})),
        ]);
        let actual: BTreeMap<Key, Actual> = [
            server(
                "posts",
                json!([{"id": "f1", "name": "title", "type": "text"}, {"id": "f2", "name": "body", "type": "text"}]),
                3,
            ),
            server("old", json!([]), 1),
        ]
        .into();
        let plan = compute(&p, &actual, &inputs(false)).unwrap();
        assert_eq!(plan.summary.create, 1);
        assert_eq!(plan.summary.update, 1);
        assert_eq!(plan.summary.delete, 0);
        assert_eq!(
            plan.unmanaged,
            vec![Unmanaged {
                kind: Kind::Collection,
                name: "old".into()
            }]
        );
        let posts = plan.changes.iter().find(|c| c.name == "posts").unwrap();
        assert_eq!(posts.destructive.len(), 2, "{posts:?}");
        assert!(posts.migration.iter().any(|m| m.contains("rebuilt")));
        assert!(posts.notes.iter().any(|n| n.contains("v3 -> v4")));
        let pruned = compute(&p, &actual, &inputs(true)).unwrap();
        assert_eq!(pruned.summary.delete, 1);
        assert_eq!(pruned.changes.last().unwrap().action, Action::Delete);
        assert!(!pruned.changes.last().unwrap().destructive.is_empty());
    }

    #[test]
    fn identical_state_plans_nothing_and_renames_are_renames() {
        let p = project(vec![coll(
            "articles",
            json!({"renamed_from": "posts", "fields": [{"name": "headline", "renamed_from": "title", "type": "text"}]}),
        )]);
        let actual: BTreeMap<Key, Actual> = [server(
            "posts",
            json!([{"id": "f1", "name": "title", "type": "text"}]),
            2,
        )]
        .into();
        let plan = compute(&p, &actual, &inputs(true)).unwrap();
        assert_eq!(plan.changes.len(), 1, "{:?}", plan.changes);
        let c = &plan.changes[0];
        assert_eq!((c.action, c.renamed_from.as_deref()), (Action::Update, Some("posts")));
        assert!(c.destructive.is_empty(), "{c:?}");
        assert!(c.notes.iter().any(|n| n.contains("renames field")));
        let same = project(vec![coll(
            "posts",
            json!({"fields": [{"name": "title", "type": "text"}]}),
        )]);
        assert!(!compute(&same, &actual, &inputs(true)).unwrap().has_changes());
    }

    #[test]
    fn relations_order_creates_and_unknown_targets_fail() {
        let p = project(vec![
            coll(
                "a_comments",
                json!({"fields": [{"name": "post", "type": "relation", "collection": "z_posts"}]}),
            ),
            coll("z_posts", json!({"fields": []})),
        ]);
        let plan = compute(&p, &BTreeMap::new(), &inputs(false)).unwrap();
        let names: Vec<&str> = plan.changes.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["z_posts", "a_comments"]);
        let bad = project(vec![coll(
            "c",
            json!({"fields": [{"name": "p", "type": "relation", "collection": "nope"}]}),
        )]);
        let err = compute(&bad, &BTreeMap::new(), &inputs(false)).unwrap_err().to_string();
        assert!(err.contains("references collection \"nope\""), "{err}");
    }

    #[test]
    fn rendering_lists_diffs_and_the_summary() {
        let p = project(vec![coll(
            "posts",
            json!({"fields": [{"name": "title", "type": "text", "max_length": 10}]}),
        )]);
        let actual: BTreeMap<Key, Actual> = [server(
            "posts",
            json!([{"id": "f1", "name": "title", "type": "text", "max_length": 5}]),
            1,
        )]
        .into();
        let plan = compute(&p, &actual, &inputs(false)).unwrap();
        let mut out = Vec::new();
        render(&plan, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("~ collection posts  [update]"), "{text}");
        assert!(text.contains("fields.title.max_length: 5 -> 10"), "{text}");
        assert!(
            text.contains("Plan: 0 to create, 1 to update, 0 to replace, 0 to delete."),
            "{text}"
        );
    }
}
