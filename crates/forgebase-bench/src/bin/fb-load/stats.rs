//! Latency statistics of one operation, and the run's report.

use std::sync::{LazyLock, Mutex};

use forgebase_bench::percentile;
use forgebase_bench::report::Report;
use serde_json::{Map, Value, json};

/// The rows of this run (written to `--json` at the end).
pub(crate) static REPORT: LazyLock<Mutex<Report>> = LazyLock::new(|| Mutex::new(Report::default()));

/// Latency samples of one operation across all workers.
#[derive(Default)]
pub(crate) struct Stats {
    pub(crate) us: Vec<u64>,
    pub(crate) errors: u64,
    /// Payload bytes moved (uploads and downloads): adds `mib_per_s`.
    pub(crate) bytes: u64,
}

impl Stats {
    pub(crate) fn merge(&mut self, o: Stats) {
        self.us.extend(o.us);
        self.errors += o.errors;
        self.bytes += o.bytes;
    }

    pub(crate) fn report(self, scenario: &str, op: &str, secs: f64) {
        self.report_with(scenario, op, secs, &[]);
    }

    /// [`Stats::report`] with more fields on the row.
    pub(crate) fn report_with(mut self, scenario: &str, op: &str, secs: f64, extra: &[(&str, Value)]) {
        let n = self.us.len() as u64;
        let (p50, p95, p99) = (
            percentile(&mut self.us, 0.50),
            percentile(&mut self.us, 0.95),
            percentile(&mut self.us, 0.99),
        );
        let mut f: Vec<(&str, Value)> = vec![
            ("n", json!(n)),
            ("errors", json!(self.errors)),
            ("rps", json!((n as f64 / secs).round())),
            ("p50_us", json!(p50)),
            ("p95_us", json!(p95)),
            ("p99_us", json!(p99)),
        ];
        if self.bytes > 0 {
            f.push((
                "mib_per_s",
                json!((self.bytes as f64 / 1_048_576.0 / secs * 10.0).round() / 10.0),
            ));
        }
        f.extend(extra.iter().map(|(k, v)| (*k, v.clone())));
        emit(scenario, op, &f);
    }
}

/// Prints a `RESULT` line and adds the row to the report.
pub(crate) fn emit(scenario: &str, op: &str, fields: &[(&str, Value)]) {
    let mut row = Map::new();
    row.insert("scenario".into(), json!(scenario));
    row.insert("op".into(), json!(op));
    let mut line = format!("RESULT scenario={scenario} op={op}");
    for (k, v) in fields {
        match v {
            Value::String(s) => line.push_str(&format!(" {k}={s}")),
            other => line.push_str(&format!(" {k}={other}")),
        }
        row.insert((*k).to_owned(), v.clone());
    }
    println!("{line}");
    REPORT.lock().unwrap().result(Value::Object(row));
}
