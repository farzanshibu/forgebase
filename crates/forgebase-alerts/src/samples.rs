//! Samples: the Prometheus text exposition parsed into series, and the
//! in-memory history that range functions (`rate`, `increase`, `delta`,
//! `*_over_time`) read.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

/// A label set, `__name__` included for raw series.
pub type Labels = BTreeMap<String, String>;

/// One series value at one evaluation.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    /// Labels (with `__name__` for a raw series).
    pub labels: Labels,
    /// Value.
    pub value: f64,
}

/// Every series of one evaluation, by metric name.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// Unix milliseconds of the evaluation.
    pub at_ms: i64,
    /// `name -> series`.
    pub series: HashMap<String, Vec<Sample>>,
}

impl Snapshot {
    /// An empty snapshot at `at_ms`.
    pub fn new(at_ms: i64) -> Self {
        Self {
            at_ms,
            series: HashMap::new(),
        }
    }

    /// Adds a series.
    pub fn push(&mut self, name: &str, labels: &[(&str, &str)], value: f64) {
        let mut l: Labels = labels.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
        l.insert("__name__".into(), name.to_owned());
        self.series
            .entry(name.to_owned())
            .or_default()
            .push(Sample { labels: l, value });
    }

    /// Parses Prometheus text format 0.0.4 into the snapshot. Lines that do
    /// not parse are skipped (the renderer is ours; this is defensive).
    pub fn add_exposition(&mut self, text: &str) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((name, labels, value)) = parse_line(line) {
                let mut l = labels;
                l.insert("__name__".into(), name.clone());
                self.series.entry(name).or_default().push(Sample { labels: l, value });
            }
        }
    }
}

fn parse_value(s: &str) -> Option<f64> {
    match s {
        "+Inf" | "Inf" => Some(f64::INFINITY),
        "-Inf" => Some(f64::NEG_INFINITY),
        "NaN" => Some(f64::NAN),
        _ => s.parse().ok(),
    }
}

/// `name{a="b",c="d"} value [timestamp]`.
fn parse_line(line: &str) -> Option<(String, Labels, f64)> {
    let name_end = line.find(|c: char| c == '{' || c.is_whitespace())?;
    let name = line[..name_end].to_owned();
    let mut labels = Labels::new();
    let mut rest = &line[name_end..];
    if rest.starts_with('{') {
        let b = rest.as_bytes();
        let mut i = 1;
        loop {
            while i < b.len() && (b[i] == b',' || b[i] == b' ') {
                i += 1;
            }
            if i >= b.len() {
                return None;
            }
            if b[i] == b'}' {
                i += 1;
                break;
            }
            let ks = i;
            while i < b.len() && b[i] != b'=' {
                i += 1;
            }
            let key = rest.get(ks..i)?.trim().to_owned();
            i += 1;
            if b.get(i) != Some(&b'"') {
                return None;
            }
            i += 1;
            let mut val = String::new();
            loop {
                match b.get(i)? {
                    b'\\' => {
                        match b.get(i + 1)? {
                            b'n' => val.push('\n'),
                            b'\\' => val.push('\\'),
                            b'"' => val.push('"'),
                            other => {
                                val.push('\\');
                                val.push(char::from(*other));
                            }
                        }
                        i += 2;
                    }
                    b'"' => {
                        i += 1;
                        break;
                    }
                    _ => {
                        // Copy one UTF-8 character.
                        let ch = rest.get(i..)?.chars().next()?;
                        val.push(ch);
                        i += ch.len_utf8();
                    }
                }
            }
            labels.insert(key, val);
        }
        rest = rest.get(i..)?;
    }
    let value = parse_value(rest.split_whitespace().next()?)?;
    Some((name, labels, value))
}

/// A series of one metric: its labels without `__name__`, sorted.
type SeriesKey = (String, Vec<(String, String)>);

/// Past values of the series that range selectors read.
#[derive(Debug, Default)]
pub struct History {
    /// Evaluation times kept, oldest first.
    times: VecDeque<i64>,
    /// `(name, labels without __name__) -> points`.
    points: HashMap<SeriesKey, VecDeque<(i64, f64)>>,
    /// Metric names kept.
    names: HashSet<String>,
    /// How far back to keep.
    keep_ms: i64,
}

/// One series over a window.
#[derive(Debug, Clone, PartialEq)]
pub struct RangeSeries {
    /// Labels (with `__name__`).
    pub labels: Labels,
    /// Points, oldest first.
    pub points: Vec<(i64, f64)>,
    /// The series first appeared after the history began, at the first
    /// point here: a counter created by its first increment started at 0
    /// one evaluation earlier, at this time.
    pub born_after: Option<i64>,
}

fn key_of(labels: &Labels) -> Vec<(String, String)> {
    labels
        .iter()
        .filter(|(k, _)| k.as_str() != "__name__")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

impl History {
    /// Keeps the metric `names` for `keep_ms` (the longest range of any rule
    /// plus one interval). Series of other names are dropped.
    pub fn configure(&mut self, names: HashSet<String>, keep_ms: i64) {
        self.names = names;
        self.keep_ms = keep_ms.max(0);
        let names = &self.names;
        self.points.retain(|(n, _), _| names.contains(n));
    }

    /// Records the series of `snap` this history keeps.
    pub fn record(&mut self, snap: &Snapshot) {
        let t = snap.at_ms;
        if self.times.back().is_some_and(|last| *last >= t) {
            return;
        }
        self.times.push_back(t);
        for name in &self.names {
            if let Some(series) = snap.series.get(name) {
                for s in series {
                    self.points
                        .entry((name.clone(), key_of(&s.labels)))
                        .or_default()
                        .push_back((t, s.value));
                }
            }
        }
        let cutoff = t - self.keep_ms;
        while self.times.front().is_some_and(|f| *f < cutoff) {
            self.times.pop_front();
        }
        self.points.retain(|_, pts| {
            while pts.front().is_some_and(|(at, _)| *at < cutoff) {
                pts.pop_front();
            }
            !pts.is_empty()
        });
    }

    /// The evaluation time before `t`, if kept.
    fn previous_time(&self, t: i64) -> Option<i64> {
        self.times.iter().rev().find(|x| **x < t).copied()
    }

    /// Points of `name` in `(at - window, at]`, per series.
    pub fn range(&self, name: &str, at_ms: i64, window_ms: i64) -> Vec<RangeSeries> {
        let from = at_ms - window_ms;
        let first_time = self.times.front().copied();
        let mut out = Vec::new();
        for ((n, key), pts) in &self.points {
            if n != name {
                continue;
            }
            let points: Vec<(i64, f64)> = pts
                .iter()
                .filter(|(t, _)| *t > from && *t <= at_ms)
                .copied()
                .collect();
            if points.is_empty() {
                continue;
            }
            let series_first = pts.front().map(|(t, _)| *t);
            let born_after = match (series_first, first_time) {
                (Some(sf), Some(ft)) if sf > ft && points.first().map(|p| p.0) == Some(sf) => {
                    self.previous_time(sf).filter(|p| *p > from)
                }
                _ => None,
            };
            let mut labels: Labels = key.iter().cloned().collect();
            labels.insert("__name__".into(), n.clone());
            out.push(RangeSeries {
                labels,
                points,
                born_after,
            });
        }
        out.sort_by(|a, b| a.labels.cmp(&b.labels));
        out
    }

    /// Number of evaluation times kept (tests, status).
    pub fn len(&self) -> usize {
        self.times.len()
    }

    /// Whether nothing is kept.
    pub fn is_empty(&self) -> bool {
        self.times.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_exposition_format() {
        let mut s = Snapshot::new(0);
        s.add_exposition(
            "# HELP x y\n# TYPE x counter\nforgebase_http_requests_total{method=\"GET\",route=\"/a\",status=\"500\"} 3\n\
             forgebase_up 1\nh_bucket{le=\"+Inf\"} 4\nq{v=\"a \\\"b\\\" \\\\ \\n é\"} NaN\nbad line here\n",
        );
        let h = &s.series["forgebase_http_requests_total"][0];
        assert_eq!(h.labels["status"], "500");
        assert_eq!(h.labels["__name__"], "forgebase_http_requests_total");
        assert_eq!(h.value, 3.0);
        assert_eq!(s.series["forgebase_up"][0].value, 1.0);
        assert_eq!(s.series["h_bucket"][0].labels["le"], "+Inf");
        assert_eq!(s.series["q"][0].labels["v"], "a \"b\" \\ \n é");
        assert!(s.series["q"][0].value.is_nan());
    }

    #[test]
    fn history_keeps_the_window_and_marks_new_series() {
        let mut h = History::default();
        h.configure(["c".to_owned()].into_iter().collect(), 60_000);
        for (t, v) in [(1_000, Some(1.0)), (2_000, Some(2.0)), (3_000, Some(5.0))] {
            let mut s = Snapshot::new(t);
            if let Some(v) = v {
                s.push("c", &[("a", "1")], v);
            }
            if t >= 2_000 {
                s.push("c", &[("a", "2")], 7.0);
            }
            h.record(&s);
        }
        let r = h.range("c", 3_000, 10_000);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].points, vec![(1_000, 1.0), (2_000, 2.0), (3_000, 5.0)]);
        assert_eq!(r[0].born_after, None);
        assert_eq!(r[1].born_after, Some(1_000), "a=2 appeared at 2000; it was 0 at 1000");
        let mut late = Snapshot::new(100_000);
        late.push("c", &[("a", "1")], 9.0);
        h.record(&late);
        assert_eq!(h.range("c", 100_000, 60_000)[0].points, vec![(100_000, 9.0)]);
        assert_eq!(h.len(), 1);
    }
}
