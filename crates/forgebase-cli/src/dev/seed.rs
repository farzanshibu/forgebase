//! Seed fixtures (`seed/<collection>.jsonl`, ADR-0037 §2) loaded into the
//! emulator: idempotent by record id, with line-numbered errors.
//!
//! A line with an `id` is stored under it. A line without one gets a
//! deterministic id (`seed-` and a digest of the collection and the line), so
//! loading twice finds the records again instead of adding copies. An
//! existing record is left alone when it already holds the line's values and
//! patched when it does not.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::bail;
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::http::{Api, describe, enc};
use crate::project::layout::{self, Project, read_seed};
use crate::project::model::{self, Kind};

/// Most passes over the failed records (a record may reference one seeded later).
const PASSES: usize = 4;

/// Counts of one collection.
#[derive(Debug, Default, Clone, Serialize, PartialEq, Eq)]
pub struct Counts {
    /// Records created.
    pub created: usize,
    /// Records patched to match the file.
    pub updated: usize,
    /// Records that already matched.
    pub unchanged: usize,
    /// Records that failed.
    pub failed: usize,
}

/// What a seed run did.
#[derive(Debug, Default, Clone, Serialize)]
pub struct Report {
    /// Per collection.
    pub collections: BTreeMap<String, Counts>,
    /// `file:line: message` of every failed record.
    pub errors: Vec<String>,
}

impl Report {
    /// Sum over the collections.
    pub fn total(&self) -> Counts {
        let mut t = Counts::default();
        for c in self.collections.values() {
            t.created += c.created;
            t.updated += c.updated;
            t.unchanged += c.unchanged;
            t.failed += c.failed;
        }
        t
    }

    /// Whether every record loaded.
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }

    /// One line per collection, then the failures.
    pub fn render(&self, out: &mut impl std::io::Write) -> std::io::Result<()> {
        for (c, n) in &self.collections {
            writeln!(
                out,
                "seed {c}: {} created, {} updated, {} unchanged{}",
                n.created,
                n.updated,
                n.unchanged,
                if n.failed > 0 {
                    format!(", {} FAILED", n.failed)
                } else {
                    String::new()
                }
            )?;
        }
        for e in &self.errors {
            writeln!(out, "  error: {e}")?;
        }
        Ok(())
    }
}

struct Item {
    collection: String,
    file: PathBuf,
    line: usize,
    id: String,
    body: Map<String, Value>,
}

/// The 1-based line numbers of the records `read_seed` returns, in order.
fn record_lines(path: &Path) -> Vec<usize> {
    std::fs::read_to_string(path)
        .map(|t| {
            t.lines()
                .enumerate()
                .filter(|(_, l)| {
                    let t = l.trim();
                    !t.is_empty() && !t.starts_with('#')
                })
                .map(|(i, _)| i + 1)
                .collect()
        })
        .unwrap_or_default()
}

/// The id a seed line is stored under.
pub fn seed_id(collection: &str, record: &Map<String, Value>) -> String {
    if let Some(Value::String(id)) = record.get("id") {
        return id.clone();
    }
    let canon = model::canonical_text(&Value::Object(record.clone()));
    let digest = Sha256::digest(format!("{collection}\n{canon}").as_bytes());
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("seed-{hex}")
}

/// Fields that cannot be compared with what the server returns (never echoed back).
fn write_only_fields(project: &Project, collection: &str) -> Vec<String> {
    let mut out = vec!["password".to_owned(), "passwordConfirm".to_owned()];
    if let Some(d) = project.get(Kind::Collection, collection) {
        for f in d.spec["fields"].as_array().into_iter().flatten() {
            if f["type"].as_str() == Some("password")
                && let Some(n) = f["name"].as_str()
            {
                out.push(n.to_owned());
            }
        }
    }
    out
}

fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        // A date the server normalised: compare by prefix of the instant.
        (Value::String(x), Value::String(y)) => {
            x == y
                || (x.len() >= 10
                    && y.strip_prefix(x.as_str()).is_some_and(|rest| {
                        rest.starts_with([' ', 'T'])
                            && rest[1..]
                                .chars()
                                .all(|c| c.is_ascii_digit() || matches!(c, ':' | '.' | 'Z' | '+' | '-'))
                    }))
        }
        _ => a == b,
    }
}

enum Done {
    Created,
    Updated,
    Unchanged,
}

async fn upsert(api: &Api, item: &Item, skip: &[String]) -> anyhow::Result<Done> {
    let base = format!("/api/v1/collections/{}/records", enc(&item.collection));
    let (status, existing) = api.call("GET", &format!("{base}/{}", enc(&item.id)), None).await?;
    match status {
        200 => {
            let mut patch = Map::new();
            for (k, v) in &item.body {
                if k == "id" || skip.contains(k) {
                    continue;
                }
                if !existing.get(k).is_some_and(|have| same(v, have)) {
                    patch.insert(k.clone(), v.clone());
                }
            }
            if patch.is_empty() {
                return Ok(Done::Unchanged);
            }
            api.ok(
                "PATCH",
                &format!("{base}/{}", enc(&item.id)),
                Some(&Value::Object(patch)),
            )
            .await?;
            Ok(Done::Updated)
        }
        404 => {
            let mut body = item.body.clone();
            body.insert("id".into(), json!(item.id));
            let (status, v) = api.call("POST", &base, Some(&Value::Object(body))).await?;
            if (200..300).contains(&status) {
                Ok(Done::Created)
            } else {
                bail!("{}", describe(status, &v))
            }
        }
        _ => bail!("{}", describe(status, &existing)),
    }
}

/// Loads every seed file of the project (ordered by collection name; records
/// that fail are retried after the others, so a relation to a collection
/// seeded later resolves).
pub async fn load(api: &Api, project: &Project) -> anyhow::Result<Report> {
    let mut report = Report::default();
    let mut items = Vec::new();
    let mut seeds: Vec<_> = project.seeds.iter().collect();
    seeds.sort_by(|a, b| a.collection.cmp(&b.collection));
    for s in seeds {
        report.collections.entry(s.collection.clone()).or_default();
        let records = match read_seed(&s.path) {
            Ok(r) => r,
            Err(e) => {
                report.errors.push(format!("{e:#}"));
                continue;
            }
        };
        let lines = record_lines(&s.path);
        for (i, r) in records.into_iter().enumerate() {
            let Value::Object(body) = r else { continue };
            items.push(Item {
                collection: s.collection.clone(),
                file: s.path.clone(),
                line: lines.get(i).copied().unwrap_or(i + 1),
                id: seed_id(&s.collection, &body),
                body,
            });
        }
    }
    let mut pending: Vec<(Item, String)> = items.into_iter().map(|i| (i, String::new())).collect();
    for _ in 0..PASSES {
        let mut failed = Vec::new();
        let mut progress = false;
        for (item, _) in pending {
            let skip = write_only_fields(project, &item.collection);
            let counts = report.collections.entry(item.collection.clone()).or_default();
            match upsert(api, &item, &skip).await {
                Ok(Done::Created) => {
                    counts.created += 1;
                    progress = true;
                }
                Ok(Done::Updated) => {
                    counts.updated += 1;
                    progress = true;
                }
                Ok(Done::Unchanged) => {
                    counts.unchanged += 1;
                    progress = true;
                }
                Err(e) => failed.push((item, format!("{e:#}"))),
            }
        }
        pending = failed;
        if pending.is_empty() || !progress {
            break;
        }
    }
    for (item, why) in pending {
        report.collections.entry(item.collection.clone()).or_default().failed += 1;
        report.errors.push(format!(
            "{}:{}: {} record {}: {why}",
            item.file.display(),
            item.line,
            item.collection,
            item.id
        ));
    }
    report.errors.sort();
    Ok(report)
}

/// Deletes every record of the project's collections (views are read-only and skipped).
pub async fn wipe(api: &Api, project: &Project) -> anyhow::Result<usize> {
    let mut names: Vec<String> = project
        .resources
        .iter()
        .filter(|r| r.kind == Kind::Collection && r.spec["type"].as_str() != Some("view"))
        .map(|r| r.name.clone())
        .collect();
    for s in &project.seeds {
        if !names.contains(&s.collection) {
            names.push(s.collection.clone());
        }
    }
    names.sort();
    let mut deleted = 0;
    for c in names {
        let base = format!("/api/v1/collections/{}/records", enc(&c));
        loop {
            let (status, page) = api
                .call("GET", &format!("{base}?per_page=200&fields=id&skip_total=true"), None)
                .await?;
            if status == 404 {
                break;
            }
            if !(200..300).contains(&status) {
                bail!("{c}: {}", describe(status, &page));
            }
            let ids: Vec<String> = page["items"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r["id"].as_str().map(str::to_owned))
                .collect();
            if ids.is_empty() {
                break;
            }
            for id in &ids {
                api.ok("DELETE", &format!("{base}/{}", enc(id)), None).await?;
                deleted += 1;
            }
        }
    }
    Ok(deleted)
}

/// Loads the project directory's seeds (reads the project from disk).
pub async fn load_dir(api: &Api, dir: &Path) -> anyhow::Result<Report> {
    let project = layout::load(dir)?;
    load(api, &project).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_come_from_the_line_or_its_content() {
        let a: Map<String, Value> = serde_json::from_str(r#"{"id":"abc","t":1}"#).unwrap();
        assert_eq!(seed_id("c", &a), "abc");
        let b: Map<String, Value> = serde_json::from_str(r#"{"t":1}"#).unwrap();
        let c: Map<String, Value> = serde_json::from_str(r#"{"t":2}"#).unwrap();
        let id = seed_id("c", &b);
        assert!(id.starts_with("seed-") && id.len() == 5 + 16);
        assert_eq!(id, seed_id("c", &b));
        assert_ne!(id, seed_id("c", &c));
        assert_ne!(id, seed_id("d", &b));
        assert!(schema_valid_id(&id));
    }

    fn schema_valid_id(id: &str) -> bool {
        (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }

    #[test]
    fn line_numbers_skip_comments_and_blank_lines() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.jsonl");
        std::fs::write(&p, "# c\n{\"a\":1}\n\n{\"a\":2}\n").unwrap();
        assert_eq!(record_lines(&p), vec![2, 4]);
    }

    #[test]
    fn comparison_ignores_number_representation() {
        assert!(same(&json!(1), &json!(1.0)));
        assert!(same(&json!({"a": [1, 2]}), &json!({"a": [1.0, 2.0]})));
        assert!(!same(&json!({"a": 1}), &json!({"a": 1, "b": 2})));
        assert!(same(&json!("2026-01-01"), &json!("2026-01-01 00:00:00.000Z")));
    }
}
