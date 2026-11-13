//! `forgebase project pull`: the server's state as a project directory.
//!
//! Files are written in a stable order and format (`name`/`type` first, then
//! keys alphabetically, defaults left out), so pulling twice gives identical
//! bytes and a diff shows only real changes. Secrets are never written: the
//! server does not return them, and function secrets become references to
//! environment variables.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde_json::{Map, Value, json};

use super::layout::{self, PROJECT_FILE};
use super::model::{self, Actual, Area, Kind};
use super::remote::{Remote, enc};

/// Options of `pull`.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Overwrite an existing project (and delete files of resources the server no longer has).
    pub force: bool,
}

/// Field keys left out when they hold their default.
fn defaults_of(ty: &str) -> Vec<(&'static str, Value)> {
    let mut d = vec![
        ("required", json!(false)),
        ("unique", json!(false)),
        ("hidden", json!(false)),
    ];
    match ty {
        "number" => d.push(("integer", json!(false))),
        "select" | "file" => d.push(("max_select", json!(1))),
        "relation" => {
            d.push(("max_select", json!(1)));
            d.push(("cascade_delete", json!(false)));
        }
        "array" => d.push(("items", json!("json"))),
        "password" => d.push(("min_length", json!(8))),
        _ => {}
    }
    d
}

fn strip_defaults(f: &Value) -> Value {
    let mut f = f.clone();
    let ty = f["type"].as_str().unwrap_or_default().to_owned();
    if let Some(o) = f.as_object_mut() {
        for (k, v) in defaults_of(&ty) {
            if o.get(k) == Some(&v) {
                o.remove(k);
            }
        }
    }
    f
}

/// The file body of a collection spec.
pub fn collection_file(name: &str, spec: &Value) -> Value {
    let mut body = spec.clone();
    if let Some(o) = body.as_object_mut() {
        if o.get("view_query").is_some_and(Value::is_null) {
            o.remove("view_query");
        }
        if let Some(Value::Array(fields)) = o.get_mut("fields") {
            *fields = fields.iter().map(strip_defaults).collect();
        }
        if let Some(Value::Array(ix)) = o.get_mut("indexes") {
            for i in ix.iter_mut() {
                if let Some(io) = i.as_object_mut() {
                    if io.get("partial").is_some_and(Value::is_null) {
                        io.remove("partial");
                    }
                    if io.get("unique") == Some(&json!(false)) {
                        io.remove("unique");
                    }
                    if io.get("kind") == Some(&json!("btree")) {
                        io.remove("kind");
                    }
                }
            }
        }
        if o.get("options").and_then(Value::as_object).is_some_and(Map::is_empty) {
            o.remove("options");
        }
    }
    layout::with_schema(Kind::Collection, name, body)
}

fn function_toml(name: &str, spec: &Value, entry: &str) -> String {
    let q = layout::toml_str;
    let list = |v: &Value| {
        let items: Vec<String> = v
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(q)
            .collect();
        format!("[{}]", items.join(", "))
    };
    let mut out = format!("#:schema ../../{}/function.schema.json\n", layout::SCHEMAS_DIR);
    if let Some(d) = spec["description"].as_str().filter(|d| !d.is_empty()) {
        out.push_str(&format!("description = {}\n", q(d)));
    }
    out.push_str(&format!(
        "access = {}\n",
        q(spec["access"].as_str().unwrap_or("permission"))
    ));
    out.push_str(&format!("entry = {}\n", q(entry)));
    out.push_str(&format!("capabilities = {}\n", list(&spec["capabilities"])));
    if spec["fetch_hosts"].as_array().is_some_and(|a| !a.is_empty()) {
        out.push_str(&format!("fetch_hosts = {}\n", list(&spec["fetch_hosts"])));
    }
    if spec
        .get("kv_rules")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
    {
        out.push_str("# Key-value namespaces beyond its own: none (deny by default).\nkv_rules = []\n");
    }
    let table = |out: &mut String, title: &str, v: &Value| {
        if let Some(m) = v.as_object().filter(|m| !m.is_empty()) {
            out.push_str(&format!("\n[{title}]\n"));
            for (k, x) in m {
                let val = match x {
                    Value::String(s) => q(s),
                    other => other.to_string(),
                };
                out.push_str(&format!("{} = {val}\n", toml_key(k)));
            }
        }
    };
    table(&mut out, "env", &spec["env"]);
    table(&mut out, "limits", &spec["limits"]);
    let secrets: Map<String, Value> = spec["secrets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|k| (k.to_owned(), json!(env_var(name, k))))
        .collect();
    if !secrets.is_empty() {
        out.push_str("\n# Secret name = environment variable that holds its value when you apply.\n");
    }
    table(&mut out, "secrets", &Value::Object(secrets));
    for h in spec["hooks"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "\n[[hooks]]\ncollection = {}\nevents = {}\n",
            q(h["collection"].as_str().unwrap_or_default()),
            list(&h["events"])
        ));
    }
    for r in spec["kv_rules"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "\n[[kv_rules]]\nnamespace = {}\naccess = {}\n",
            q(r["namespace"].as_str().unwrap_or_default()),
            q(r["access"].as_str().unwrap_or_default())
        ));
    }
    out
}

fn toml_key(k: &str) -> String {
    if !k.is_empty() && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
        k.to_owned()
    } else {
        layout::toml_str(k)
    }
}

/// The environment variable pull names for a function secret.
pub fn env_var(function: &str, key: &str) -> String {
    let up = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect()
    };
    format!("{}_{}", up(function), up(key))
}

/// What pull wrote.
#[derive(Debug, Default)]
pub struct Report {
    /// Files written.
    pub written: Vec<PathBuf>,
    /// Stale files removed (`--force`).
    pub removed: Vec<PathBuf>,
    /// Notes (functions without downloadable code, ...).
    pub warnings: Vec<String>,
}

struct Writer {
    dir: PathBuf,
    report: Report,
}

impl Writer {
    fn write(&mut self, rel: &Path, bytes: &[u8]) -> anyhow::Result<()> {
        let p = self.dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
        }
        std::fs::write(&p, bytes).with_context(|| format!("cannot write {}", p.display()))?;
        self.report.written.push(rel.to_owned());
        Ok(())
    }

    fn json(&mut self, rel: PathBuf, v: &Value) -> anyhow::Result<()> {
        self.write(&rel, model::pretty(v).as_bytes())
    }
}

/// Pulls the server state into `dir`.
pub async fn pull(remote: &Remote, dir: &Path, opts: &Options, out: &mut impl Write) -> anyhow::Result<Report> {
    let project_file = dir.join(PROJECT_FILE);
    let existing = project_file.is_file();
    if existing && !opts.force {
        bail!(
            "{} already holds a project: pass --force to overwrite its resource files with the server's state",
            dir.display()
        );
    }
    // The areas to pull: those the existing project manages, or all.
    let manage: Vec<Area> = if existing {
        let text = std::fs::read_to_string(&project_file)?;
        let t: toml::Table = toml::from_str(&text).with_context(|| format!("{}", project_file.display()))?;
        match t.get("manage").and_then(toml::Value::as_array) {
            Some(a) => a
                .iter()
                .filter_map(toml::Value::as_str)
                .filter_map(Area::parse)
                .collect(),
            None => Area::ALL.to_vec(),
        }
    } else {
        Area::ALL.to_vec()
    };
    let kinds: std::collections::BTreeSet<Kind> =
        Kind::ALL.into_iter().filter(|k| manage.contains(&k.area())).collect();
    let state = remote.fetch_all(&kinds).await?;
    let mut w = Writer {
        dir: dir.to_owned(),
        report: Report::default(),
    };
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    if !existing {
        let name = std::path::absolute(dir)
            .ok()
            .and_then(|p| {
                let base = if p.file_name().and_then(|n| n.to_str()) == Some("forgebase") {
                    p.parent().map(Path::to_path_buf)
                } else {
                    Some(p)
                };
                base.and_then(|b| b.file_name().and_then(|n| n.to_str()).map(str::to_owned))
            })
            .unwrap_or_else(|| "forgebase-project".into());
        w.write(
            Path::new(PROJECT_FILE),
            layout::project_file(&name, Some(&remote.base)).as_bytes(),
        )?;
    }
    layout::write_schemas(dir)?;
    let mut policies: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
    let mut count = 0_usize;
    for ((kind, name), a) in &state {
        let customized = a.spec["fields"].as_array().is_some_and(|f| !f.is_empty());
        if model::is_builtin(*kind, name) && !(*kind == Kind::Collection && customized) {
            continue;
        }
        count += 1;
        let stem = layout::encode_stem(name);
        let rel = |ext: &str| Path::new(layout::kind_dir(*kind)).join(format!("{stem}.{ext}"));
        match kind {
            Kind::Collection => w.json(rel("json"), &collection_file(name, &a.spec))?,
            Kind::Bucket | Kind::Topic | Kind::Subscription | Kind::OauthClient => {
                w.json(rel("json"), &layout::with_schema(*kind, name, a.spec.clone()))?;
            }
            Kind::Function => pull_function(remote, &mut w, name, a).await?,
            Kind::Policy => {
                let (rk, rn) = name.split_once('/').unwrap_or((name, ""));
                policies
                    .entry(rk.to_owned())
                    .or_default()
                    .insert(rn.to_owned(), a.spec["rules"].clone());
            }
        }
    }
    for (rk, rules) in &policies {
        let body = json!({"$schema": layout::schema_ref(Kind::Policy), "rules": rules});
        w.json(
            Path::new(layout::kind_dir(Kind::Policy)).join(format!("{rk}.json")),
            &body,
        )?;
    }
    if existing {
        remove_stale(&mut w, &kinds)?;
    }

    let mut report = w.report;
    writeln!(
        out,
        "pulled {} resource{} from {} into {}",
        count,
        if count == 1 { "" } else { "s" },
        remote.base,
        dir.display()
    )?;
    for r in &report.removed {
        writeln!(out, "  removed {} (not on the server)", dir.join(r).display())?;
    }
    for wn in &report.warnings {
        writeln!(out, "  warning: {wn}")?;
    }
    report.written.sort();
    Ok(report)
}

async fn pull_function(remote: &Remote, w: &mut Writer, name: &str, a: &Actual) -> anyhow::Result<()> {
    let fdir = Path::new(layout::kind_dir(Kind::Function)).join(name);
    let version = a.raw["active_version"].as_i64().unwrap_or(0);
    let base = format!("/api/v1/functions/{}/versions/{version}", enc(name));
    let language = a.spec["code"]["language"].as_str().unwrap_or("wasm");
    let entry = match language {
        "typescript" => "index.ts",
        "javascript" => "index.js",
        _ => "module.wasm",
    };
    if language == "wasm" {
        match remote.get_bytes(&format!("{base}/module")).await? {
            Some(bytes) => w.write(&fdir.join(entry), &bytes)?,
            None => w.report.warnings.push(format!(
                "function {name}: this server cannot download WebAssembly modules; copy the module to {}",
                w.dir.join(&fdir).join(entry).display()
            )),
        }
    } else {
        let src = remote.call("GET", &format!("{base}/source"), None).await?;
        w.write(&fdir.join(entry), src["source"].as_str().unwrap_or_default().as_bytes())?;
    }
    w.write(
        &fdir.join("function.toml"),
        function_toml(name, &a.spec, entry).as_bytes(),
    )
}

/// Removes resource files of pulled kinds that the pull did not write.
fn remove_stale(w: &mut Writer, kinds: &std::collections::BTreeSet<Kind>) -> anyhow::Result<()> {
    let written: std::collections::BTreeSet<PathBuf> = w.report.written.iter().cloned().collect();
    for kind in kinds {
        let root = w.dir.join(layout::kind_dir(*kind));
        let Ok(rd) = std::fs::read_dir(&root) else { continue };
        for e in rd.filter_map(Result::ok) {
            let p = e.path();
            let rel = Path::new(layout::kind_dir(*kind)).join(e.file_name());
            if *kind == Kind::Function {
                if p.is_dir() && !written.contains(&rel.join("function.toml")) {
                    std::fs::remove_dir_all(&p)?;
                    w.report.removed.push(rel);
                }
            } else if p.extension().and_then(|x| x.to_str()) == Some("json") && !written.contains(&rel) {
                std::fs::remove_file(&p)?;
                w.report.removed.push(rel);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn collection_files_leave_defaults_out_and_load_back_equal() {
        let spec = model::collection_spec(&json!({
            "fields": [{"name": "title", "type": "text", "required": true}, {"name": "tags", "type": "select", "values": ["a"]}],
            "indexes": [{"name": "ix", "keys": [{"path": "title"}]}],
            "rules": {"list_rule": ""}
        }));
        let file = collection_file("posts", &spec);
        assert_eq!(
            file["fields"][1],
            json!({"name": "tags", "type": "select", "values": ["a"]})
        );
        assert_eq!(file["indexes"][0], json!({"name": "ix", "keys": [{"path": "title"}]}));
        assert_eq!(file["$schema"], "../.schemas/collection.schema.json");
        assert_eq!(model::collection_spec(&file), spec);
    }

    #[test]
    fn function_files_reference_secrets_by_variable() {
        let spec = model::function_spec(
            "d",
            "public",
            &json!({"capabilities": ["log"], "env": {"A": "1"}, "limits": {"fuel": 5}, "hooks": [{"collection": "c", "events": ["after_create"]}]}),
            ("typescript", "x"),
            &BTreeSet::from(["API_KEY".to_owned()]),
            Some(&json!([])),
        );
        let text = function_toml("send-mail", &spec, "index.ts");
        assert!(text.contains("API_KEY = \"SEND_MAIL_API_KEY\""), "{text}");
        assert!(
            text.contains("[[hooks]]\ncollection = \"c\"\nevents = [\"after_create\"]"),
            "{text}"
        );
        let parsed: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(parsed["limits"]["fuel"].as_integer(), Some(5));
        assert_eq!(parsed["kv_rules"].as_array().map(Vec::len), Some(0));
    }
}
