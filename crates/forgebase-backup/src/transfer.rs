//! Export and import of collections (schema and records) and of the
//! configuration, preserving collection, field and record ids.
//!
//! An export is a directory:
//!
//! ```text
//! bundle.json                    format, versions, per-collection counts and SHA-256
//! collections/<name>.schema.json the collection definition (ids included)
//! collections/<name>.ndjson      one record per line, `id` included
//! config.toml                    effective configuration (optional)
//! ```
//!
//! Import creates missing collections with their original ids, in an order
//! that satisfies relations, then inserts records with their original ids in
//! batches of 200 (each batch atomic). `created`/`updated`/version values are
//! assigned afresh by the data service, as in the REST import. Records of auth
//! collections are not exported (password hashes never leave the database);
//! views carry their definition only.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use forgebase_core::{ErrorCode, ErrorKind};
use forgebase_data::schema::{Collection, CollectionType};
use forgebase_data::{AccessContext, DataService, ListQuery};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};

use crate::error::{BackupError, Result, io};
use crate::fsutil::{join_rel, mkdir_private, sha256_file, write_atomic};

const PAGE: u32 = 500;
const BATCH: usize = 200;
/// Bundle format version.
pub const BUNDLE_FORMAT: u32 = 1;
const BUNDLE: &str = "bundle.json";

/// What to export.
#[derive(Debug, Clone, Default)]
pub struct ExportOptions {
    /// Only these collections (all non-system collections when `None`).
    pub collections: Option<Vec<String>>,
    /// Effective configuration as TOML, written to `config.toml`.
    pub config_toml: Option<String>,
}

/// One collection of a bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleCollection {
    /// Collection name.
    pub name: String,
    /// Collection id.
    pub id: String,
    /// `base`, `auth` or `view`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Records in the NDJSON file.
    pub records: u64,
    /// SHA-256 of the NDJSON file (absent for views and auth collections).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/// `bundle.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bundle {
    /// Bundle format.
    pub format: u32,
    /// RFC 3339 creation time.
    pub created: String,
    /// ForgeBase version.
    pub forgebase_version: String,
    /// Collections in dependency order.
    pub collections: Vec<BundleCollection>,
    /// Whether `config.toml` is present.
    #[serde(default)]
    pub config: bool,
}

/// Result of an export.
#[derive(Debug, Clone, Serialize)]
pub struct ExportReport {
    /// What was written.
    pub bundle: Bundle,
    /// Findings (skipped records, unresolved relations).
    pub warnings: Vec<String>,
}

fn safe_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 128 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Orders collections so relation targets come first; cycles keep their order.
fn dependency_order(mut colls: Vec<Collection>) -> Vec<Collection> {
    let ids: HashSet<String> = colls.iter().map(|c| c.id.clone()).collect();
    let mut done: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(colls.len());
    while !colls.is_empty() {
        let before = colls.len();
        let mut rest = Vec::new();
        for c in colls {
            let ready = c.fields.iter().all(|f| {
                f.kind
                    .relation_target()
                    .is_none_or(|t| t == c.id || !ids.contains(t) || done.contains(t))
            });
            if ready {
                done.insert(c.id.clone());
                out.push(c);
            } else {
                rest.push(c);
            }
        }
        colls = rest;
        if colls.len() == before {
            out.append(&mut colls);
        }
    }
    out
}

/// Exports collections to the new directory `dest`.
pub async fn export_bundle(data: &DataService, dest: &Path, opts: ExportOptions) -> Result<ExportReport> {
    let ctx = AccessContext::system();
    if dest.exists() {
        return Err(BackupError::Conflict(format!("{} already exists", dest.display())));
    }
    let all = data.list_collections(&ctx).await?;
    let mut chosen: Vec<Collection> = match &opts.collections {
        Some(names) => {
            let mut v = Vec::new();
            for n in names {
                let c = all
                    .iter()
                    .find(|c| &c.name == n || &c.id == n)
                    .ok_or_else(|| BackupError::NotFound(format!("collection {n}")))?;
                if c.system {
                    return Err(BackupError::Invalid(format!("{n} is a system collection")));
                }
                v.push(c.clone());
            }
            v
        }
        None => all.into_iter().filter(|c| !c.system).collect(),
    };
    chosen = dependency_order(chosen);

    let colls_dir = dest.join("collections");
    mkdir_private(&colls_dir).map_err(io(&colls_dir))?;
    let mut warnings = Vec::new();
    let mut entries = Vec::new();
    let names: HashSet<String> = chosen.iter().map(|c| c.id.clone()).collect();
    for c in &chosen {
        if !safe_name(&c.name) {
            return Err(BackupError::Invalid(format!(
                "collection name {:?} cannot be used as a file name",
                c.name
            )));
        }
        for f in &c.fields {
            if let Some(t) = f.kind.relation_target() {
                if !names.contains(t) {
                    warnings.push(format!(
                        "{}.{} relates to a collection that is not part of this export",
                        c.name, f.name
                    ));
                }
            }
        }
        let schema = serde_json::to_vec_pretty(c).map_err(|e| BackupError::Internal(e.to_string()))?;
        write_atomic(&colls_dir.join(format!("{}.schema.json", c.name)), &schema)?;

        let kind = match c.kind {
            CollectionType::Base => "base",
            CollectionType::Auth => "auth",
            CollectionType::View => "view",
        };
        let mut entry = BundleCollection {
            name: c.name.clone(),
            id: c.id.clone(),
            kind: kind.to_owned(),
            records: 0,
            sha256: None,
        };
        match c.kind {
            CollectionType::View => {}
            CollectionType::Auth => warnings.push(format!(
                "{}: records of an auth collection are not exported (password hashes stay in the database)",
                c.name
            )),
            CollectionType::Base => {
                let path = colls_dir.join(format!("{}.ndjson", c.name));
                entry.records = write_records(data, &ctx, &c.name, &path).await?;
                entry.sha256 = Some(sha256_file(&path)?.0);
            }
        }
        entries.push(entry);
    }
    let config = opts.config_toml.is_some();
    if let Some(toml) = &opts.config_toml {
        write_atomic(&dest.join("config.toml"), toml.as_bytes())?;
    }
    let bundle = Bundle {
        format: BUNDLE_FORMAT,
        created: forgebase_core::now_rfc3339(),
        forgebase_version: env!("CARGO_PKG_VERSION").to_owned(),
        collections: entries,
        config,
    };
    let json = serde_json::to_vec_pretty(&bundle).map_err(|e| BackupError::Internal(e.to_string()))?;
    write_atomic(&dest.join(BUNDLE), &json)?;
    Ok(ExportReport { bundle, warnings })
}

async fn write_records(data: &DataService, ctx: &AccessContext, collection: &str, path: &Path) -> Result<u64> {
    let file = crate::fsutil::create_private(path).map_err(io(path))?;
    let mut w = BufWriter::new(file);
    let mut query = ListQuery {
        per_page: Some(PAGE),
        skip_total: true,
        sort: Some("id".into()),
        ..ListQuery::default()
    };
    let mut count = 0u64;
    loop {
        let page = data.list_records(ctx, collection, &query).await?;
        for rec in &page.items {
            let line = serde_json::to_vec(rec).map_err(|e| BackupError::Internal(e.to_string()))?;
            w.write_all(&line).map_err(io(path))?;
            w.write_all(b"\n").map_err(io(path))?;
            count += 1;
        }
        match page.next_cursor {
            Some(c) => query.cursor = Some(c),
            None => break,
        }
    }
    w.flush().map_err(io(path))?;
    w.get_ref().sync_all().map_err(io(path))?;
    Ok(count)
}

/// What to do with data that already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnConflict {
    /// Stop with an error at an existing collection or record.
    #[default]
    Fail,
    /// Reuse existing collections and skip records whose id exists.
    Skip,
}

/// Import settings.
#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// Existing collections and records.
    pub on_conflict: OnConflict,
    /// Only these collections.
    pub collections: Option<Vec<String>>,
    /// Check the bundle and report what would happen, change nothing.
    pub dry_run: bool,
}

/// Per-collection import result.
#[derive(Debug, Clone, Serialize)]
pub struct ImportedCollection {
    /// Collection name.
    pub name: String,
    /// The collection was created by this import.
    pub created: bool,
    /// Records inserted.
    pub records: u64,
    /// Records skipped because their id existed.
    pub skipped: u64,
}

/// Result of an import.
#[derive(Debug, Clone, Serialize)]
pub struct ImportReport {
    /// One entry per collection, in import order.
    pub collections: Vec<ImportedCollection>,
    /// Findings.
    pub warnings: Vec<String>,
    /// Nothing was changed.
    pub dry_run: bool,
}

/// Reads and checks `bundle.json` and the hashes of its record files.
pub fn read_bundle(src: &Path) -> Result<Bundle> {
    let path = src.join(BUNDLE);
    let text = std::fs::read_to_string(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            BackupError::Format(format!(
                "{} is not an export bundle ({BUNDLE} is missing)",
                src.display()
            ))
        } else {
            io(&path)(e)
        }
    })?;
    let b: Bundle = serde_json::from_str(&text).map_err(|e| BackupError::Format(format!("{BUNDLE}: {e}")))?;
    if b.format != BUNDLE_FORMAT {
        return Err(BackupError::Format(format!("unsupported bundle format {}", b.format)));
    }
    for c in &b.collections {
        if !safe_name(&c.name) {
            return Err(BackupError::Format(format!("unsafe collection name {:?}", c.name)));
        }
        if let Some(want) = &c.sha256 {
            let p = join_rel(src, &format!("collections/{}.ndjson", c.name));
            let (got, _) = sha256_file(&p)?;
            if &got != want {
                return Err(BackupError::Format(format!(
                    "{} is corrupt (SHA-256 differs from {BUNDLE})",
                    p.display()
                )));
            }
        }
    }
    Ok(b)
}

/// Imports the bundle at `src`.
pub async fn import_bundle(data: &DataService, src: &Path, opts: ImportOptions) -> Result<ImportReport> {
    let ctx = AccessContext::system();
    let bundle = read_bundle(src)?;
    let wanted: Option<HashSet<&str>> = opts
        .collections
        .as_ref()
        .map(|v| v.iter().map(String::as_str).collect());
    let mut report = ImportReport {
        collections: Vec::new(),
        warnings: Vec::new(),
        dry_run: opts.dry_run,
    };
    if let Some(w) = &wanted {
        for n in w {
            if !bundle.collections.iter().any(|c| c.name == *n) {
                return Err(BackupError::NotFound(format!("collection {n} in the bundle")));
            }
        }
    }
    let mut known: HashMap<String, bool> = HashMap::new();
    for entry in &bundle.collections {
        if wanted.as_ref().is_some_and(|w| !w.contains(entry.name.as_str())) {
            continue;
        }
        let schema_path = src.join("collections").join(format!("{}.schema.json", entry.name));
        let text = std::fs::read_to_string(&schema_path).map_err(io(&schema_path))?;
        let def: Collection =
            serde_json::from_str(&text).map_err(|e| BackupError::Format(format!("{}: {e}", schema_path.display())))?;
        let exists = data.get_collection(&ctx, &entry.name).await.is_ok();
        known.insert(entry.name.clone(), exists);
        if exists && opts.on_conflict == OnConflict::Fail {
            return Err(BackupError::Conflict(format!(
                "collection {} already exists (use --on-conflict skip to merge into it)",
                entry.name
            )));
        }
        let mut item = ImportedCollection {
            name: entry.name.clone(),
            created: false,
            records: 0,
            skipped: 0,
        };
        if opts.dry_run {
            item.created = !exists;
            item.records = entry.records;
            report.collections.push(item);
            continue;
        }
        if !exists {
            data.create_collection(&ctx, def).await?;
            item.created = true;
        }
        if entry.kind == "base" {
            let path = src.join("collections").join(format!("{}.ndjson", entry.name));
            let (n, skipped) = import_records(data, &ctx, &entry.name, &path, opts.on_conflict).await?;
            item.records = n;
            item.skipped = skipped;
        }
        report.collections.push(item);
    }
    Ok(report)
}

fn clean(mut rec: Map<String, Json>) -> Json {
    for k in ["created", "updated", "_version", "expand"] {
        rec.remove(k);
    }
    Json::Object(rec)
}

async fn import_records(
    data: &DataService,
    ctx: &AccessContext,
    collection: &str,
    path: &Path,
    on_conflict: OnConflict,
) -> Result<(u64, u64)> {
    let file = std::fs::File::open(path).map_err(io(path))?;
    let records = BufReader::new(file)
        .lines()
        .enumerate()
        .filter_map(|(n, line)| match line {
            Err(e) => Some(Err(io(path)(e))),
            Ok(l) if l.trim().is_empty() => None,
            Ok(l) => Some(
                serde_json::from_str::<Map<String, Json>>(&l)
                    .map_err(|e| BackupError::Format(format!("{} line {}: {e}", path.display(), n + 1))),
            ),
        });
    import_stream(data, ctx, collection, records, on_conflict).await
}

async fn import_stream(
    data: &DataService,
    ctx: &AccessContext,
    collection: &str,
    records: impl Iterator<Item = Result<Map<String, Json>>>,
    on_conflict: OnConflict,
) -> Result<(u64, u64)> {
    let mut batch: Vec<Json> = Vec::with_capacity(BATCH);
    let (mut inserted, mut skipped) = (0u64, 0u64);
    for rec in records {
        batch.push(clean(rec?));
        if batch.len() == BATCH {
            flush(
                data,
                ctx,
                collection,
                &mut batch,
                on_conflict,
                &mut inserted,
                &mut skipped,
            )
            .await?;
        }
    }
    flush(
        data,
        ctx,
        collection,
        &mut batch,
        on_conflict,
        &mut inserted,
        &mut skipped,
    )
    .await?;
    Ok((inserted, skipped))
}

/// File format of a single-collection export or import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// One JSON array.
    Json,
    /// One JSON object per line.
    Ndjson,
    /// Comma-separated values (export only).
    Csv,
}

impl std::str::FromStr for Format {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s {
            "json" => Ok(Self::Json),
            "ndjson" | "jsonl" => Ok(Self::Ndjson),
            "csv" => Ok(Self::Csv),
            other => Err(format!("unknown format {other:?}; use json, ndjson or csv")),
        }
    }
}

fn csv_cell(v: Option<&Json>) -> String {
    let text = match v {
        None | Some(Json::Null) => String::new(),
        Some(Json::String(s)) => s.clone(),
        Some(Json::Bool(b)) => b.to_string(),
        Some(Json::Number(n)) => n.to_string(),
        Some(other) => other.to_string(),
    };
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text
    }
}

/// Exports one collection to the file `dest` as JSON, NDJSON or CSV. CSV
/// columns are `id`, `created`, `updated` and the declared fields (never
/// passwords); non-scalar values are written as JSON text.
pub async fn export_records(data: &DataService, collection: &str, dest: &Path, format: Format) -> Result<u64> {
    use forgebase_data::schema::FieldKind;
    let ctx = AccessContext::system();
    if dest.exists() {
        return Err(BackupError::Conflict(format!("{} already exists", dest.display())));
    }
    let coll = data.get_collection(&ctx, collection).await?;
    if coll.kind == CollectionType::Auth {
        return Err(BackupError::Invalid(format!(
            "{collection} is an auth collection; its records are not exported"
        )));
    }
    let columns: Vec<String> = ["id", "created", "updated"]
        .into_iter()
        .map(str::to_owned)
        .chain(
            coll.fields
                .iter()
                .filter(|f| !matches!(f.kind, FieldKind::Password { .. }))
                .map(|f| f.name.clone()),
        )
        .collect();
    let file = crate::fsutil::create_private(dest).map_err(io(dest))?;
    let mut w = BufWriter::new(file);
    let put = |w: &mut BufWriter<std::fs::File>, bytes: &[u8]| w.write_all(bytes).map_err(io(dest));
    match format {
        Format::Json => put(&mut w, b"[")?,
        Format::Csv => put(&mut w, format!("{}\n", columns.join(",")).as_bytes())?,
        Format::Ndjson => {}
    }
    let mut query = ListQuery {
        per_page: Some(PAGE),
        skip_total: true,
        sort: Some("id".into()),
        ..ListQuery::default()
    };
    let mut count = 0u64;
    loop {
        let page = data.list_records(&ctx, collection, &query).await?;
        for rec in &page.items {
            match format {
                Format::Json => {
                    put(&mut w, if count == 0 { b"\n" } else { b",\n" })?;
                    put(
                        &mut w,
                        &serde_json::to_vec(rec).map_err(|e| BackupError::Internal(e.to_string()))?,
                    )?;
                }
                Format::Ndjson => {
                    put(
                        &mut w,
                        &serde_json::to_vec(rec).map_err(|e| BackupError::Internal(e.to_string()))?,
                    )?;
                    put(&mut w, b"\n")?;
                }
                Format::Csv => {
                    let line: Vec<String> = columns.iter().map(|c| csv_cell(rec.get(c))).collect();
                    put(&mut w, format!("{}\n", line.join(",")).as_bytes())?;
                }
            }
            count += 1;
        }
        match page.next_cursor {
            Some(c) => query.cursor = Some(c),
            None => break,
        }
    }
    if format == Format::Json {
        put(&mut w, b"\n]\n")?;
    }
    w.flush().map_err(io(dest))?;
    w.get_ref().sync_all().map_err(io(dest))?;
    Ok(count)
}

/// Imports records from a JSON (array) or NDJSON file into an existing
/// collection, keeping their ids. Returns `(inserted, skipped)`.
pub async fn import_records_file(
    data: &DataService,
    collection: &str,
    src: &Path,
    format: Format,
    on_conflict: OnConflict,
) -> Result<(u64, u64)> {
    let ctx = AccessContext::system();
    data.get_collection(&ctx, collection).await?;
    match format {
        Format::Ndjson => import_records(data, &ctx, collection, src, on_conflict).await,
        Format::Json => {
            let text = std::fs::read_to_string(src).map_err(io(src))?;
            let items: Vec<Map<String, Json>> =
                serde_json::from_str(&text).map_err(|e| BackupError::Format(format!("{}: {e}", src.display())))?;
            import_stream(data, &ctx, collection, items.into_iter().map(Ok), on_conflict).await
        }
        Format::Csv => Err(BackupError::Invalid(
            "CSV import is available over REST (POST /api/v1/collections/{name}/import); use json or ndjson here"
                .into(),
        )),
    }
}

/// A record whose id (or another unique value) is already taken.
fn is_duplicate(e: &forgebase_data::DataError) -> bool {
    match e {
        forgebase_data::DataError::Validation(v) => {
            v.fields.contains_key("id") || v.fields.keys().any(|k| k.ends_with(".id"))
        }
        other => other.kind() == ErrorKind::Conflict,
    }
}

async fn flush(
    data: &DataService,
    ctx: &AccessContext,
    collection: &str,
    batch: &mut Vec<Json>,
    on_conflict: OnConflict,
    inserted: &mut u64,
    skipped: &mut u64,
) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let items = std::mem::take(batch);
    let len = items.len() as u64;
    match data.create_records(ctx, collection, items.clone()).await {
        Ok(_) => *inserted += len,
        Err(e) if on_conflict == OnConflict::Skip && is_duplicate(&e) => {
            for item in items {
                match data.create_record(ctx, collection, item).await {
                    Ok(_) => *inserted += 1,
                    Err(e) if is_duplicate(&e) => *skipped += 1,
                    Err(e) => return Err(e.into()),
                }
            }
        }
        Err(e) => {
            return Err(BackupError::Format(format!(
                "importing into {collection} failed after {inserted} record(s): {e}"
            )));
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ config

/// The effective configuration as TOML (what `forgebase export config` writes).
/// The data directory is machine specific and left out. Secrets are not part
/// of the configuration (they come from the environment or are sealed in the
/// database), so there is nothing to redact.
pub fn config_to_toml(config: &forgebase_core::Config) -> Result<String> {
    let mut value = toml::Value::try_from(config).map_err(|e| BackupError::Internal(format!("config to TOML: {e}")))?;
    if let Some(t) = value.as_table_mut() {
        t.remove("data_dir");
    }
    toml::to_string_pretty(&value).map_err(|e| BackupError::Internal(format!("config to TOML: {e}")))
}

/// Validates the configuration file at `path` exactly as the server would
/// (unknown keys and out-of-range values are errors) without reading the
/// environment.
pub fn validate_config_file(path: &Path) -> Result<forgebase_core::Config> {
    use forgebase_core::config::{ConfigLoader, ConfigOverrides};
    let cwd = path
        .parent()
        .map_or_else(|| std::path::PathBuf::from("."), Path::to_path_buf);
    let loaded = ConfigLoader::new(cwd)
        .cli(ConfigOverrides {
            config_file: Some(path.to_path_buf()),
            ..ConfigOverrides::default()
        })
        .load()
        .map_err(|e| BackupError::Invalid(format!("{}: {e}", path.display())))?;
    Ok(loaded.config)
}
