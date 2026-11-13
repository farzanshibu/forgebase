//! The project directory (ADR-0037 §2): reading and validating it.
//!
//! ```text
//! forgebase/                         --dir, default ./forgebase
//! ├── forgebase.project.toml         format, name, manage, [defaults], [environments.<name>]
//! ├── .schemas/*.schema.json         JSON Schemas for editors (generated)
//! ├── collections/<name>.json        fields, indexes, rules
//! ├── functions/<name>/function.toml + the entry (index.ts, index.js, module.wasm, ...)
//! ├── queues/topics/<name>.json
//! ├── queues/subscriptions/<name>.json
//! ├── storage/buckets/<name>.json
//! ├── policies/<resource kind>.json  engine resource rules
//! ├── oauth-clients/<name>.json
//! └── seed/<collection>.jsonl        fixtures for `forgebase dev --seed` (never applied)
//! ```
//!
//! Every error names the file and line. All files are checked before the
//! first error is reported, so one run lists every problem.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::bail;
use serde_json::{Value, json};

use super::model::{self, Area, Code, Desired, Extra, FunctionExtra, Kind};
use super::schema::{self, Seg};

/// Name of the project file.
pub const PROJECT_FILE: &str = "forgebase.project.toml";
/// Directory of the generated JSON Schemas.
pub const SCHEMAS_DIR: &str = ".schemas";
/// Format version this CLI reads and writes.
pub const FORMAT: i64 = 1;
/// Largest function source (the server's limit).
const MAX_SOURCE: u64 = 1 << 20;
/// Largest WebAssembly module the CLI uploads.
const MAX_WASM: u64 = 64 << 20;
/// Entry files tried in order when `entry` is not set.
pub const DEFAULT_ENTRIES: &[&str] = &[
    "index.ts",
    "index.mts",
    "index.js",
    "index.mjs",
    "module.wasm",
    "main.wasm",
];

/// Relative directory of each file-per-resource kind.
pub fn kind_dir(kind: Kind) -> &'static str {
    match kind {
        Kind::Collection => "collections",
        Kind::Bucket => "storage/buckets",
        Kind::Topic => "queues/topics",
        Kind::Subscription => "queues/subscriptions",
        Kind::Function => "functions",
        Kind::Policy => "policies",
        Kind::OauthClient => "oauth-clients",
    }
}

/// Schema file of each kind.
pub fn kind_schema(kind: Kind) -> &'static str {
    match kind {
        Kind::Collection => "collection.schema.json",
        Kind::Bucket => "bucket.schema.json",
        Kind::Topic => "topic.schema.json",
        Kind::Subscription => "subscription.schema.json",
        Kind::Function => "function.schema.json",
        Kind::Policy => "policy.schema.json",
        Kind::OauthClient => "oauth-client.schema.json",
    }
}

/// Connection settings of `[defaults]` or `[environments.<name>]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Target {
    /// Server URL.
    pub url: Option<String>,
    /// Realm.
    pub realm: Option<String>,
    /// ForgeBase project id or slug.
    pub project: Option<String>,
    /// ForgeBase environment name.
    pub environment: Option<String>,
}

impl Target {
    fn from(v: Option<&Value>) -> Self {
        let get = |k: &str| v.and_then(|v| v.get(k)).and_then(Value::as_str).map(str::to_owned);
        Self {
            url: get("url"),
            realm: get("realm"),
            project: get("project"),
            environment: get("environment"),
        }
    }

    /// `self` with unset values taken from `base`.
    pub fn over(&self, base: &Target) -> Target {
        Target {
            url: self.url.clone().or_else(|| base.url.clone()),
            realm: self.realm.clone().or_else(|| base.realm.clone()),
            project: self.project.clone().or_else(|| base.project.clone()),
            environment: self.environment.clone().or_else(|| base.environment.clone()),
        }
    }
}

/// `forgebase.project.toml`.
#[derive(Debug, Clone)]
pub struct ProjectConfig {
    /// Project name.
    pub name: String,
    /// Areas the project owns.
    pub manage: BTreeSet<Area>,
    /// `[defaults]`.
    pub defaults: Target,
    /// `[environments.<name>]`.
    pub environments: BTreeMap<String, Target>,
}

impl ProjectConfig {
    /// The connection target for `--env` (merged over `[defaults]`; the
    /// environment name defaults to the table name, or to `env` itself when
    /// there is no table).
    pub fn target(&self, env: Option<&str>) -> Target {
        match env {
            None => self.defaults.clone(),
            Some(e) => {
                let mut t = self.environments.get(e).cloned().unwrap_or_default();
                if t.environment.is_none() {
                    t.environment = Some(e.to_owned());
                }
                t.over(&self.defaults)
            }
        }
    }

    /// Whether a kind is owned by the project.
    pub fn manages(&self, kind: Kind) -> bool {
        self.manage.contains(&kind.area())
    }
}

/// A seed fixture file.
#[derive(Debug, Clone)]
pub struct SeedFile {
    /// Target collection.
    pub collection: String,
    /// The file.
    pub path: PathBuf,
    /// Number of records.
    pub records: usize,
}

/// A loaded project.
#[derive(Debug, Clone)]
pub struct Project {
    /// The project directory.
    pub dir: PathBuf,
    /// The project file.
    pub config: ProjectConfig,
    /// Every declared resource.
    pub resources: Vec<Desired>,
    /// Seed fixtures (validated, not applied).
    pub seeds: Vec<SeedFile>,
    /// Non-fatal findings.
    pub warnings: Vec<String>,
}

impl Project {
    /// A declared resource.
    pub fn get(&self, kind: Kind, name: &str) -> Option<&Desired> {
        self.resources.iter().find(|r| r.kind == kind && r.name == name)
    }
}

/// Collected errors of one load.
struct Errors {
    dir: PathBuf,
    items: Vec<String>,
}

impl Errors {
    fn at(&mut self, rel: &Path, line: Option<usize>, message: impl std::fmt::Display) {
        let file = self.dir.join(rel);
        match line {
            Some(l) => self.items.push(format!("{}:{l}: {message}", file.display())),
            None => self.items.push(format!("{}: {message}", file.display())),
        }
    }
}

/// Parsed file text and the way to locate paths in it.
struct Source {
    rel: PathBuf,
    text: String,
    toml: bool,
}

impl Source {
    fn line(&self, path: &[Seg]) -> Option<usize> {
        if self.toml {
            schema::locate_toml(&self.text, path)
        } else {
            schema::locate_json(&self.text, path)
        }
    }

    fn report(&self, errs: &mut Errors, path: &[Seg], message: impl std::fmt::Display) {
        let at = schema::path_text(path);
        let line = self.line(path);
        if at.is_empty() {
            errs.at(&self.rel, line, message);
        } else {
            errs.at(&self.rel, line, format!("{at}: {message}"));
        }
    }
}

fn read_text(dir: &Path, rel: &Path, errs: &mut Errors) -> Option<String> {
    match std::fs::read_to_string(dir.join(rel)) {
        Ok(t) => Some(t),
        Err(e) => {
            errs.at(rel, None, format!("cannot read: {e}"));
            None
        }
    }
}

/// Parses and validates a JSON file against a schema file of `.schemas/`.
fn json_file(dir: &Path, rel: &Path, schema_file: &str, errs: &mut Errors) -> Option<(Source, Value)> {
    let text = read_text(dir, rel, errs)?;
    let value: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            errs.at(rel, Some(e.line()), format!("invalid JSON: {e}"));
            return None;
        }
    };
    let src = Source {
        rel: rel.to_owned(),
        text,
        toml: false,
    };
    let violations = schema::validate(&schema::by_file(schema_file), &value);
    if !violations.is_empty() {
        for v in violations {
            src.report(errs, &v.path, &v.message);
        }
        return None;
    }
    Some((src, value))
}

/// Parses and validates a TOML file (as JSON) against a schema file.
fn toml_file(dir: &Path, rel: &Path, schema_file: &str, errs: &mut Errors) -> Option<(Source, Value)> {
    let text = read_text(dir, rel, errs)?;
    let table: toml::Table = match toml::from_str(&text) {
        Ok(t) => t,
        Err(e) => {
            let line = e
                .span()
                .map(|s| text[..s.start.min(text.len())].matches('\n').count() + 1);
            errs.at(rel, line, format!("invalid TOML: {}", e.message()));
            return None;
        }
    };
    let value = match serde_json::to_value(&table) {
        Ok(v) => v,
        Err(e) => {
            errs.at(
                rel,
                None,
                format!("unsupported TOML value (dates are not used in project files): {e}"),
            );
            return None;
        }
    };
    let src = Source {
        rel: rel.to_owned(),
        text,
        toml: true,
    };
    let violations = schema::validate(&schema::by_file(schema_file), &value);
    if !violations.is_empty() {
        for v in violations {
            src.report(errs, &v.path, &v.message);
        }
        return None;
    }
    Some((src, value))
}

/// The files of a directory with an extension, sorted; empty when it does not exist.
fn files_with(dir: &Path, rel: &str, ext: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir.join(rel))
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some(ext))
                .filter_map(|p| p.file_name().map(|n| Path::new(rel).join(n)))
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

fn stem(rel: &Path) -> String {
    let raw = rel.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    decode_stem(raw)
}

/// File name of a resource name: characters outside `[A-Za-z0-9._-]` are
/// percent-encoded (topic names may hold `:`).
pub fn encode_stem(name: &str) -> String {
    name.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn decode_stem(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(h) = s.get(i + 1..i + 3)
            && let Ok(v) = u8::from_str_radix(h, 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_owned())
}

/// The resource name: the file's `name` (when present) or the file name.
fn name_of(src: &Source, v: &Value, errs: &mut Errors) -> String {
    let from_file = stem(&src.rel);
    match v.get("name").and_then(Value::as_str) {
        Some(n) if n != from_file => {
            src.report(
                errs,
                &[Seg::Key("name".into())],
                format!("name {n:?} does not match the file name {from_file:?} (rename the file or the resource)"),
            );
            n.to_owned()
        }
        Some(n) => n.to_owned(),
        None => from_file,
    }
}

fn queue_name_ok(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 128
        && n.as_bytes()[0].is_ascii_alphanumeric()
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

/// Function names: a lower-case letter, then lower-case letters, digits, `_` or `-` (at most 48).
pub fn function_name_ok(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 48
        && n.starts_with(|c: char| c.is_ascii_lowercase())
        && n.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        && n != "types"
}

fn env_name_ok(n: &str) -> bool {
    !n.is_empty()
        && n.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn load_config(dir: &Path, errs: &mut Errors) -> Option<ProjectConfig> {
    let rel = Path::new(PROJECT_FILE);
    if !dir.join(rel).is_file() {
        errs.at(
            rel,
            None,
            "not found: run `forgebase new <dir>` or `forgebase project pull` to create a project, or pass --dir",
        );
        return None;
    }
    let (src, v) = toml_file(dir, rel, "project.schema.json", errs)?;
    let manage = match v.get("manage").and_then(Value::as_array) {
        Some(items) => items.iter().filter_map(Value::as_str).filter_map(Area::parse).collect(),
        None => Area::ALL.into_iter().collect(),
    };
    let environments = v
        .get("environments")
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, t)| (k.clone(), Target::from(Some(t)))).collect())
        .unwrap_or_default();
    let config = ProjectConfig {
        name: v.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
        manage,
        defaults: Target::from(v.get("defaults")),
        environments,
    };
    for (path, t) in std::iter::once((vec![Seg::Key("defaults".into())], &config.defaults)).chain(
        config
            .environments
            .iter()
            .map(|(k, t)| (vec![Seg::Key("environments".into()), Seg::Key(k.clone())], t)),
    ) {
        if let Some(u) = &t.url
            && !(u.starts_with("http://") || u.starts_with("https://"))
        {
            src.report(errs, &path, format!("url {u:?} must start with http:// or https://"));
        }
    }
    Some(config)
}

/// Loads and validates the project in `dir`.
pub fn load(dir: &Path) -> anyhow::Result<Project> {
    let mut errs = Errors {
        dir: dir.to_owned(),
        items: Vec::new(),
    };
    if !dir.is_dir() {
        bail!(
            "{} is not a directory: run `forgebase new <dir>` or `forgebase project pull`, or pass --dir",
            dir.display()
        );
    }
    let config = load_config(dir, &mut errs);
    let mut resources: Vec<Desired> = Vec::new();
    let mut warnings = Vec::new();
    load_collections(dir, &mut errs, &mut resources);
    load_simple(dir, Kind::Bucket, &mut errs, &mut resources);
    load_simple(dir, Kind::Topic, &mut errs, &mut resources);
    load_simple(dir, Kind::Subscription, &mut errs, &mut resources);
    load_simple(dir, Kind::OauthClient, &mut errs, &mut resources);
    load_functions(dir, &mut errs, &mut resources);
    load_policies(dir, &mut errs, &mut resources);
    let collections: BTreeSet<String> = resources
        .iter()
        .filter(|r| r.kind == Kind::Collection)
        .map(|r| r.name.clone())
        .collect();
    let seeds = load_seeds(dir, &mut errs, &collections, &mut warnings);
    // Duplicates (two files naming the same resource).
    let mut seen: BTreeMap<(Kind, String), PathBuf> = BTreeMap::new();
    for r in &resources {
        if let Some(prev) = seen.insert((r.kind, r.name.clone()), r.file.clone()) {
            errs.at(
                &r.file,
                None,
                format!(
                    "{} {:?} is also declared in {}",
                    r.kind,
                    r.name,
                    dir.join(prev).display()
                ),
            );
        }
    }
    // Index names are global on the server.
    let mut index_owner: BTreeMap<String, String> = BTreeMap::new();
    for r in resources.iter().filter(|r| r.kind == Kind::Collection) {
        for ix in r.spec.get("indexes").and_then(Value::as_array).into_iter().flatten() {
            let n = ix["name"].as_str().unwrap_or_default().to_owned();
            if let Some(other) = index_owner.insert(n.clone(), r.name.clone())
                && other != r.name
            {
                errs.at(
                    &r.file,
                    None,
                    format!("index {n:?} is also declared by collection {other:?}"),
                );
            }
        }
    }
    if !errs.items.is_empty() {
        let n = errs.items.len();
        bail!(
            "{}\n{n} problem{} in the project",
            errs.items.join("\n"),
            if n == 1 { "" } else { "s" }
        );
    }
    let config = config.ok_or_else(|| anyhow::anyhow!("no project file"))?;
    resources.sort_by(|a, b| (a.kind, &a.name).cmp(&(b.kind, &b.name)));
    for r in &mut resources {
        model::canon_numbers(&mut r.spec);
    }
    Ok(Project {
        dir: dir.to_owned(),
        config,
        resources,
        seeds,
        warnings,
    })
}

fn load_collections(dir: &Path, errs: &mut Errors, out: &mut Vec<Desired>) {
    for rel in files_with(dir, kind_dir(Kind::Collection), "json") {
        let Some((src, v)) = json_file(dir, &rel, kind_schema(Kind::Collection), errs) else {
            continue;
        };
        let name = name_of(&src, &v, errs);
        if let Err(e) = forgebase_data::schema::validate_collection_name(&name, false) {
            src.report(errs, &[], format!("collection name {name:?}: {e}"));
        }
        let view = v.get("type").and_then(Value::as_str) == Some("view");
        if view
            && v.get("view_query")
                .and_then(Value::as_str)
                .is_none_or(|q| q.trim().is_empty())
        {
            src.report(errs, &[], "a view collection needs a view_query");
        }
        if !view && v.get("view_query").is_some_and(|q| !q.is_null()) {
            src.report(
                errs,
                &[Seg::Key("view_query".into())],
                "only view collections have a view_query",
            );
        }
        if view && v.get("fields").and_then(Value::as_array).is_some_and(|f| !f.is_empty()) {
            src.report(
                errs,
                &[Seg::Key("fields".into())],
                "view fields come from the query; do not declare them",
            );
        }
        let mut renames = BTreeMap::new();
        let mut names = BTreeSet::new();
        for (i, f) in v
            .get("fields")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let fname = f.get("name").and_then(Value::as_str).unwrap_or_default();
            let at = [Seg::Key("fields".into()), Seg::Index(i), Seg::Key("name".into())];
            if let Err(e) = forgebase_data::schema::validate_field_name(fname) {
                src.report(errs, &at, format!("field name {fname:?}: {e}"));
            }
            if !names.insert(fname.to_ascii_lowercase()) {
                src.report(errs, &at, format!("field {fname:?} is declared twice"));
            }
            if let Some(old) = f.get("renamed_from").and_then(Value::as_str) {
                renames.insert(fname.to_owned(), old.to_owned());
            }
        }
        let mut ix_names = BTreeSet::new();
        for (i, ix) in v
            .get("indexes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let n = ix.get("name").and_then(Value::as_str).unwrap_or_default();
            if !ix_names.insert(n.to_owned()) {
                src.report(
                    errs,
                    &[Seg::Key("indexes".into()), Seg::Index(i)],
                    format!("index {n:?} is declared twice"),
                );
            }
            if ix.get("keys").and_then(Value::as_array).is_some_and(Vec::is_empty) {
                src.report(
                    errs,
                    &[Seg::Key("indexes".into()), Seg::Index(i), Seg::Key("keys".into())],
                    "an index needs at least one key",
                );
            }
        }
        out.push(Desired {
            kind: Kind::Collection,
            name,
            spec: model::collection_spec(&v),
            file: rel,
            extra: Extra::Collection {
                renamed_from: v.get("renamed_from").and_then(Value::as_str).map(str::to_owned),
                field_renames: renames,
            },
        });
    }
}

fn load_simple(dir: &Path, kind: Kind, errs: &mut Errors, out: &mut Vec<Desired>) {
    for rel in files_with(dir, kind_dir(kind), "json") {
        let Some((src, v)) = json_file(dir, &rel, kind_schema(kind), errs) else {
            continue;
        };
        let name = name_of(&src, &v, errs);
        let (spec, extra) = match kind {
            Kind::Bucket => (model::bucket_spec(&v, v.get("acl")), Extra::None),
            Kind::Topic => {
                if !queue_name_ok(&name) {
                    src.report(errs, &[], format!("topic name {name:?}: 1-128 letters, digits, '.', '_', ':' or '-', starting with a letter or digit"));
                }
                (model::topic_spec(&v, true), Extra::None)
            }
            Kind::Subscription => {
                if !queue_name_ok(&name) {
                    src.report(errs, &[], format!("subscription name {name:?}: 1-128 letters, digits, '.', '_', ':' or '-', starting with a letter or digit"));
                }
                let push = v.get("push").filter(|p| p.is_object());
                let secret_env = push
                    .and_then(|p| p.get("secret_env"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if let Some(e) = &secret_env
                    && !env_name_ok(e)
                {
                    src.report(
                        errs,
                        &[Seg::Key("push".into()), Seg::Key("secret_env".into())],
                        format!("{e:?} is not an environment variable name"),
                    );
                }
                if let Some(p) = push
                    && p.get("kind").and_then(Value::as_str) == Some("webhook")
                    && !p
                        .get("endpoint")
                        .and_then(Value::as_str)
                        .is_some_and(|e| e.starts_with("https://") || e.starts_with("http://"))
                {
                    src.report(
                        errs,
                        &[Seg::Key("push".into()), Seg::Key("endpoint".into())],
                        "a webhook endpoint is an https:// URL",
                    );
                }
                (
                    model::subscription_spec(&v, true),
                    Extra::Subscription {
                        start: v.get("start").cloned(),
                        secret_env,
                    },
                )
            }
            Kind::OauthClient => {
                let secret_env = v.get("secret_env").and_then(Value::as_str).map(str::to_owned);
                if let Some(e) = &secret_env
                    && !env_name_ok(e)
                {
                    src.report(
                        errs,
                        &[Seg::Key("secret_env".into())],
                        format!("{e:?} is not an environment variable name"),
                    );
                }
                (model::oauth_spec(&v, true), Extra::OauthClient { secret_env })
            }
            _ => continue,
        };
        out.push(Desired {
            kind,
            name,
            spec,
            file: rel,
            extra,
        });
    }
}

/// Reads a function's code: `.ts`/`.mts` TypeScript, `.js`/`.mjs` JavaScript, `.wasm` a module.
pub fn read_code(path: &Path) -> anyhow::Result<Code> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() {
        bail!("not a file");
    }
    let language = match ext.as_str() {
        "ts" | "mts" => "typescript",
        "js" | "mjs" => "javascript",
        "wasm" => {
            if meta.len() > MAX_WASM {
                bail!("{} bytes; the CLI uploads modules up to {MAX_WASM}", meta.len());
            }
            let bytes = std::fs::read(path)?;
            if !bytes.starts_with(b"\0asm") {
                bail!("not a WebAssembly binary (no \\0asm magic)");
            }
            return Ok(Code::Wasm(bytes));
        }
        other => bail!("unknown entry type {other:?}: use .ts, .mts, .js, .mjs or .wasm"),
    };
    if meta.len() > MAX_SOURCE {
        bail!("{} bytes; sources are limited to {MAX_SOURCE}", meta.len());
    }
    let text = std::fs::read_to_string(path)?;
    Ok(Code::Source { language, text })
}

fn load_functions(dir: &Path, errs: &mut Errors, out: &mut Vec<Desired>) {
    let root = dir.join(kind_dir(Kind::Function));
    let Ok(rd) = std::fs::read_dir(&root) else { return };
    let mut dirs: Vec<PathBuf> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for fdir in dirs {
        let dname = fdir.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
        let rel = Path::new(kind_dir(Kind::Function)).join(&dname).join("function.toml");
        if !dir.join(&rel).is_file() {
            errs.at(
                &rel,
                None,
                "missing: every functions/<name>/ directory needs a function.toml",
            );
            continue;
        }
        let Some((src, v)) = toml_file(dir, &rel, kind_schema(Kind::Function), errs) else {
            continue;
        };
        let name = match v.get("name").and_then(Value::as_str) {
            Some(n) if n != dname => {
                src.report(
                    errs,
                    &[Seg::Key("name".into())],
                    format!("name {n:?} does not match the directory name {dname:?}"),
                );
                n.to_owned()
            }
            Some(n) => n.to_owned(),
            None => dname.clone(),
        };
        if !function_name_ok(&name) {
            src.report(
                errs,
                &[],
                format!(
                    "function name {name:?}: a lower-case letter, then lower-case letters, digits, _ or - (at most 48)"
                ),
            );
        }
        let build = v.get("build").and_then(Value::as_str).map(str::to_owned);
        let entry_rel = match v.get("entry").and_then(Value::as_str) {
            Some(e) => {
                let p = Path::new(e);
                if p.is_absolute() || p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
                    src.report(
                        errs,
                        &[Seg::Key("entry".into())],
                        "entry is a path inside the function directory",
                    );
                }
                PathBuf::from(e)
            }
            None => DEFAULT_ENTRIES
                .iter()
                .map(PathBuf::from)
                .find(|e| fdir.join(e).is_file())
                .unwrap_or_else(|| PathBuf::from("index.ts")),
        };
        let entry = fdir.join(&entry_rel);
        let code = if entry.is_file() {
            match read_code(&entry) {
                Ok(c) => Some(c),
                Err(e) => {
                    src.report(
                        errs,
                        &[Seg::Key("entry".into())],
                        format!("{}: {e:#}", entry_rel.display()),
                    );
                    None
                }
            }
        } else if build.is_some() {
            None
        } else {
            src.report(
                errs,
                &[Seg::Key("entry".into())],
                format!(
                    "entry {} does not exist (set `entry`, add one of {}, or a `build` command that writes it)",
                    entry_rel.display(),
                    DEFAULT_ENTRIES.join(", ")
                ),
            );
            None
        };
        let secrets: BTreeMap<String, String> = v
            .get("secrets")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(k, x)| (k.clone(), x.as_str().unwrap_or_default().to_owned()))
                    .collect()
            })
            .unwrap_or_default();
        for (k, e) in &secrets {
            if !env_name_ok(e) {
                src.report(
                    errs,
                    &[Seg::Key("secrets".into()), Seg::Key(k.clone())],
                    format!("{e:?} is not an environment variable name"),
                );
            }
        }
        let names: BTreeSet<String> = secrets.keys().cloned().collect();
        let (lang, sha) = match &code {
            Some(c) => (c.language(), c.sha256()),
            None => ("pending", "pending build".to_owned()),
        };
        let spec = model::function_spec(
            v.get("description").and_then(Value::as_str).unwrap_or_default(),
            v.get("access").and_then(Value::as_str).unwrap_or("permission"),
            &v,
            (lang, &sha),
            &names,
            v.get("kv_rules"),
        );
        out.push(Desired {
            kind: Kind::Function,
            name,
            spec,
            file: rel,
            extra: Extra::Function(Box::new(FunctionExtra {
                dir: fdir.clone(),
                entry,
                build,
                code,
                secrets,
            })),
        });
    }
}

/// Engine resource kinds a `policies/<kind>.json` file may name.
pub const POLICY_KINDS: &[&str] = &[
    "kv_namespace",
    "vector_collection",
    "graph_label",
    "graph_edge_type",
    "bucket",
    "queue_topic",
    "queue_subscription",
    "realtime_channel",
];

fn load_policies(dir: &Path, errs: &mut Errors, out: &mut Vec<Desired>) {
    for rel in files_with(dir, kind_dir(Kind::Policy), "json") {
        let Some((src, v)) = json_file(dir, &rel, kind_schema(Kind::Policy), errs) else {
            continue;
        };
        let rkind = stem(&rel);
        if !POLICY_KINDS.contains(&rkind.as_str()) {
            src.report(
                errs,
                &[],
                format!(
                    "{rkind:?} is not a resource kind; name the file one of {}",
                    POLICY_KINDS.join(", ")
                ),
            );
            continue;
        }
        for (name, rules) in v["rules"].as_object().into_iter().flatten() {
            out.push(Desired {
                kind: Kind::Policy,
                name: format!("{rkind}/{name}"),
                spec: model::policy_spec(rules),
                file: rel.clone(),
                extra: Extra::None,
            });
        }
    }
}

/// Reads the records of a seed file (`seed/<collection>.jsonl`): one JSON
/// object per non-empty line; `#` starts a comment line. Errors name the line.
pub fn read_seed(path: &Path) -> anyhow::Result<Vec<Value>> {
    let text = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        match serde_json::from_str::<Value>(t) {
            Ok(v @ Value::Object(_)) => out.push(v),
            Ok(_) => bail!("{}:{}: a seed line is one JSON object", path.display(), i + 1),
            Err(e) => bail!("{}:{}: invalid JSON: {e}", path.display(), i + 1),
        }
    }
    Ok(out)
}

fn load_seeds(
    dir: &Path,
    errs: &mut Errors,
    collections: &BTreeSet<String>,
    warnings: &mut Vec<String>,
) -> Vec<SeedFile> {
    let mut out = Vec::new();
    for rel in files_with(dir, "seed", "jsonl") {
        let collection = stem(&rel);
        match read_seed(&dir.join(&rel)) {
            Ok(records) => {
                if !collections.contains(&collection) {
                    warnings.push(format!(
                        "{}: no collections/{collection}.json in the project (fine for a collection the server already has, such as users)",
                        dir.join(&rel).display()
                    ));
                }
                out.push(SeedFile {
                    collection,
                    path: dir.join(&rel),
                    records: records.len(),
                });
            }
            Err(e) => errs.items.push(format!("{e:#}")),
        }
    }
    out
}

/// The `$schema` value of a JSON file of `kind` (relative to the file).
pub fn schema_ref(kind: Kind) -> String {
    let depth = kind_dir(kind).matches('/').count() + 1;
    format!("{}{SCHEMAS_DIR}/{}", "../".repeat(depth), kind_schema(kind))
}

/// Writes `.schemas/*.schema.json`.
pub fn write_schemas(dir: &Path) -> anyhow::Result<()> {
    let sdir = dir.join(SCHEMAS_DIR);
    std::fs::create_dir_all(&sdir)?;
    for s in schema::all() {
        std::fs::write(sdir.join(s.file), model::pretty(&s.schema))?;
    }
    Ok(())
}

/// A `forgebase.project.toml` text.
pub fn project_file(name: &str, url: Option<&str>) -> String {
    let mut out = format!(
        "#:schema {SCHEMAS_DIR}/project.schema.json\n# ForgeBase project (docs/project-as-code.md).\nformat = {FORMAT}\nname = {}\n\n# Resource kinds this project owns; kinds left out are not changed or reported.\nmanage = [{}]\n",
        toml_str(name),
        model::Area::ALL
            .iter()
            .map(|a| toml_str(a.as_str()))
            .collect::<Vec<_>>()
            .join(", ")
    );
    out.push_str("\n# Defaults for every command; --server / FORGEBASE_URL override the url.\n[defaults]\n");
    match url {
        Some(u) => out.push_str(&format!("url = {}\n", toml_str(u))),
        None => out.push_str("# url = \"http://127.0.0.1:8090\"\n"),
    }
    out.push_str(
        "# realm = \"default\"\n\n# Targets for --env NAME (merged over [defaults]).\n# [environments.production]\n# url = \"https://api.example.com\"\n",
    );
    out
}

/// A TOML basic string.
pub fn toml_str(s: &str) -> String {
    toml::Value::String(s.to_owned()).to_string()
}

/// The JSON body of a resource file as pull writes it: `$schema` first, then the spec.
pub fn with_schema(kind: Kind, name: &str, body: Value) -> Value {
    let mut out = json!({"$schema": schema_ref(kind), "name": name});
    if let (Some(o), Value::Object(b)) = (out.as_object_mut(), body) {
        for (k, v) in b {
            o.insert(k, v);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn base(dir: &Path) {
        write(dir, PROJECT_FILE, &project_file("demo", Some("http://127.0.0.1:1")));
    }

    #[test]
    fn a_valid_project_loads_every_kind() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        base(d);
        write(
            d,
            "collections/posts.json",
            r#"{"fields": [{"name": "title", "type": "text", "required": true}], "indexes": [], "rules": {"list_rule": ""}}"#,
        );
        write(d, "storage/buckets/uploads.json", r#"{"visibility": "public"}"#);
        write(d, "queues/topics/orders%3Anew.json", r#"{"labels": {"a": "b"}}"#);
        write(
            d,
            "queues/subscriptions/s1.json",
            r#"{"topic": "orders:new", "push": {"kind": "webhook", "endpoint": "https://example.com/h", "secret_env": "S1_SECRET"}}"#,
        );
        write(
            d,
            "functions/hello/function.toml",
            "access = \"public\"\ncapabilities = [\"log\"]\n[secrets]\nKEY = \"HELLO_KEY\"\n",
        );
        write(d, "functions/hello/index.ts", "export default 1;\n");
        write(d, "policies/bucket.json", r#"{"rules": {"uploads": {"read": ""}}}"#);
        write(
            d,
            "oauth-clients/web.json",
            r#"{"type": "public", "redirect_uris": ["https://app/cb"]}"#,
        );
        write(
            d,
            "seed/posts.jsonl",
            "{\"title\": \"a\"}\n\n# comment\n{\"title\": \"b\"}\n",
        );
        let p = load(d).unwrap();
        let kinds: Vec<(Kind, &str)> = p.resources.iter().map(|r| (r.kind, r.name.as_str())).collect();
        assert_eq!(
            kinds,
            vec![
                (Kind::Collection, "posts"),
                (Kind::Bucket, "uploads"),
                (Kind::Topic, "orders:new"),
                (Kind::Function, "hello"),
                (Kind::Subscription, "s1"),
                (Kind::Policy, "bucket/uploads"),
                (Kind::OauthClient, "web"),
            ]
        );
        assert_eq!(p.seeds[0].records, 2);
        let f = p.get(Kind::Function, "hello").unwrap();
        assert_eq!(f.spec["code"]["language"], "typescript");
        assert_eq!(f.spec["secrets"], json!(["KEY"]));
        assert_eq!(p.config.target(None).url.as_deref(), Some("http://127.0.0.1:1"));
        assert_eq!(p.config.target(Some("staging")).environment.as_deref(), Some("staging"));
    }

    #[test]
    fn every_problem_is_reported_with_file_and_line() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        base(d);
        write(
            d,
            "collections/posts.json",
            "{\n  \"fields\": [\n    {\"name\": \"title\", \"type\": \"text\",\n     \"maxlength\": 3}\n  ]\n}\n",
        );
        write(d, "storage/buckets/b.json", "{\"visibility\": \"public\",}");
        write(
            d,
            "functions/hello/function.toml",
            "access = \"public\"\n[limits]\nfuel = 1\nspeed = 2\n",
        );
        write(d, "functions/other/function.toml", "access = \"public\"\n");
        write(d, "seed/posts.jsonl", "{\"a\": 1}\n[1]\n");
        let err = load(d).unwrap_err().to_string();
        let root = d.display().to_string();
        assert!(
            err.contains(&format!(
                "{root}/collections/posts.json:4: fields[0].maxlength: unknown key"
            )),
            "{err}"
        );
        assert!(
            err.contains(&format!("{root}/storage/buckets/b.json:1: invalid JSON")),
            "{err}"
        );
        assert!(
            err.contains(&format!(
                "{root}/functions/hello/function.toml:4: limits.speed: unknown key"
            )),
            "{err}"
        );
        assert!(
            err.contains("function.toml:") && err.contains("entry index.ts does not exist"),
            "{err}"
        );
        assert!(
            err.contains("seed/posts.jsonl:2: a seed line is one JSON object"),
            "{err}"
        );
        assert!(err.contains("5 problems"), "{err}");
    }

    #[test]
    fn names_must_match_files_and_be_unique() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        base(d);
        write(d, "collections/posts.json", r#"{"name": "articles"}"#);
        write(d, "collections/bad-name.json", r#"{}"#);
        let err = load(d).unwrap_err().to_string();
        assert!(err.contains("does not match the file name"), "{err}");
        assert!(err.contains("collection name \"bad-name\""), "{err}");
    }

    #[test]
    fn stems_round_trip() {
        assert_eq!(encode_stem("orders:new"), "orders%3Anew");
        assert_eq!(decode_stem("orders%3Anew"), "orders:new");
        assert_eq!(decode_stem("100%"), "100%");
        assert_eq!(schema_ref(Kind::Topic), "../../.schemas/topic.schema.json");
        assert_eq!(schema_ref(Kind::Collection), "../.schemas/collection.schema.json");
    }
}
