//! The JSON report of an `fb-load` run, the file CI stores and compares
//! (`crates/forgebase-bench/baseline/`, `scripts/compare.py`).
//!
//! ```json
//! {
//!   "format": 1,
//!   "tool": "fb-load",
//!   "label": "linux-postgres",
//!   "backend": "postgres",
//!   "version": "0.3.5",
//!   "started_at_unix": 1790000000,
//!   "host": {"os": "linux", "arch": "x86_64", "cpus": 4},
//!   "config": {"concurrency": 16, "duration_secs": 10, ...},
//!   "results": [{"scenario": "records", "op": "create", "n": 1, "errors": 0,
//!                "rps": 1.0, "p50_us": 1, "p95_us": 1, "p99_us": 1}],
//!   "resources": [{"scenario": "records", "samples": 10, "cpu_pct": {"p50": 1, "max": 2}, ...}],
//!   "extra": {"topology": {...}}
//! }
//! ```
//!
//! `results` rows are keyed by `scenario/op`; every row has `n` and `errors`,
//! most have `rps` and the latency percentiles in microseconds. The format
//! number changes only when a field changes meaning.

use serde_json::{Map, Value, json};

/// Version of the report format.
pub const FORMAT: u32 = 1;

/// Collects the rows of one run.
#[derive(Debug, Default)]
pub struct Report {
    /// One object per measured operation.
    pub results: Vec<Value>,
    /// One object per sampled scenario.
    pub resources: Vec<Value>,
    /// Scenario-specific documents (topology: lag series, read distribution).
    pub extra: Map<String, Value>,
}

impl Report {
    /// Adds an operation row.
    pub fn result(&mut self, row: Value) {
        self.results.push(row);
    }

    /// Adds a resource summary.
    pub fn resource(&mut self, row: Value) {
        self.resources.push(row);
    }

    /// Adds a scenario-specific document under `key`.
    pub fn extra(&mut self, key: &str, v: Value) {
        self.extra.insert(key.to_owned(), v);
    }

    /// The whole document. `config` is the run's arguments, `label` and
    /// `backend` are free text the caller names (CI passes both).
    pub fn document(&self, label: &str, backend: &str, config: Value) -> Value {
        json!({
            "format": FORMAT,
            "tool": "fb-load",
            "label": label,
            "backend": backend,
            "version": env!("CARGO_PKG_VERSION"),
            "started_at_unix": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            "host": {
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "cpus": std::thread::available_parallelism().map_or(0, std::num::NonZeroUsize::get),
            },
            "config": config,
            "results": self.results,
            "resources": self.resources,
            "extra": self.extra,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_has_the_documented_shape() {
        let mut r = Report::default();
        r.result(json!({"scenario": "records", "op": "create", "n": 3, "errors": 0}));
        r.resource(json!({"scenario": "records", "samples": 2}));
        r.extra("topology", json!({"reads": 1}));
        let d = r.document("t", "sqlite", json!({"concurrency": 4}));
        assert_eq!(d["format"], 1);
        assert_eq!(d["tool"], "fb-load");
        assert_eq!(d["backend"], "sqlite");
        assert_eq!(d["results"][0]["op"], "create");
        assert_eq!(d["resources"][0]["samples"], 2);
        assert_eq!(d["extra"]["topology"]["reads"], 1);
        assert!(d["host"]["cpus"].as_u64().is_some());
    }
}
