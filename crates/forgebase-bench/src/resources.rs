//! Resource sampling of a running server (FB-81 follow-up): CPU, resident
//! memory, open file descriptors and data-directory disk usage, once a
//! second, over the process **tree** of the server (the bundled PostgreSQL is
//! a child of the server, so a PostgreSQL run counts the engine too).
//!
//! No dependency: on Linux the numbers come from `/proc` (clock ticks, `VmRSS`,
//! `/proc/<pid>/fd`); elsewhere (macOS, the laptop runs) from `ps` and `lsof`.
//! The two readers differ in precision (Linux reads CPU ticks, 10 ms; macOS
//! `ps` reads centiseconds of CPU time) and in cost (`lsof` is slow, so open
//! descriptors are best effort off Linux); the report says which.
//!
//! CPU is a percentage of **one core** (two busy cores read 200).
#![allow(clippy::cast_precision_loss)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// One reading of the process tree.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    /// CPU seconds (user + system) used so far by the whole tree.
    pub cpu_secs: f64,
    /// Resident memory of the whole tree, in KiB.
    pub rss_kb: u64,
    /// Open descriptors of the whole tree (`None`: not available here).
    pub open_fds: Option<u64>,
    /// Processes in the tree.
    pub procs: u64,
}

/// One processed sample.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    /// CPU, percent of one core, since the previous sample.
    pub cpu_pct: f64,
    /// Resident memory in KiB.
    pub rss_kb: u64,
    /// Open descriptors.
    pub open_fds: Option<u64>,
    /// Processes in the tree.
    pub procs: u64,
    /// Bytes the data directory occupies on disk.
    pub disk_bytes: u64,
}

/// Parses `ps` CPU time: `[[d-]hh:]mm:ss[.cc]` (also `mm:ss.cc` on macOS).
pub fn parse_cputime(s: &str) -> Option<f64> {
    let s = s.trim();
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<f64>().ok()?, r),
        None => (0.0, s),
    };
    let mut total = 0.0;
    for part in rest.split(':') {
        total = total * 60.0 + part.parse::<f64>().ok()?;
    }
    Some(days * 86_400.0 + total)
}

/// One row of `ps -Ao pid=,ppid=,cputime=,rss=`.
#[derive(Debug, Clone, PartialEq)]
pub struct PsRow {
    /// Process id.
    pub pid: u32,
    /// Parent process id.
    pub ppid: u32,
    /// CPU seconds.
    pub cpu_secs: f64,
    /// Resident KiB.
    pub rss_kb: u64,
}

/// Parses the output of `ps -Ao pid=,ppid=,cputime=,rss=`.
pub fn parse_ps(text: &str) -> Vec<PsRow> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            Some(PsRow {
                pid: it.next()?.parse().ok()?,
                ppid: it.next()?.parse().ok()?,
                cpu_secs: parse_cputime(it.next()?)?,
                rss_kb: it.next()?.parse().ok()?,
            })
        })
        .collect()
}

/// `root` and every descendant of it among `rows`.
pub fn tree_of(root: u32, rows: &[PsRow]) -> Vec<PsRow> {
    let mut keep: Vec<u32> = vec![root];
    loop {
        let before = keep.len();
        for r in rows {
            if keep.contains(&r.ppid) && !keep.contains(&r.pid) {
                keep.push(r.pid);
            }
        }
        if keep.len() == before {
            break;
        }
    }
    rows.iter().filter(|r| keep.contains(&r.pid)).cloned().collect()
}

#[cfg(target_os = "linux")]
fn read_proc(pid: u32) -> Option<(PsRow, u64)> {
    // /proc/<pid>/stat: `pid (comm) state ppid ... utime stime ...`; comm may
    // hold spaces and parentheses, so split after the last `)`.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    let ppid: u32 = f.get(1)?.parse().ok()?;
    let (ut, st): (f64, f64) = (f.get(11)?.parse().ok()?, f.get(12)?.parse().ok()?);
    let rss_kb = std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("VmRSS:"))
                .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
        })
        .unwrap_or(0);
    let fds = std::fs::read_dir(format!("/proc/{pid}/fd")).map_or(0, |d| d.count() as u64);
    // CLK_TCK is 100 on every Linux target we run on.
    Some((
        PsRow {
            pid,
            ppid,
            cpu_secs: (ut + st) / 100.0,
            rss_kb,
        },
        fds,
    ))
}

/// Reads the tree under `root`.
#[cfg(target_os = "linux")]
pub fn snapshot(root: u32) -> Option<Snapshot> {
    let mut rows = Vec::new();
    let mut fds = std::collections::HashMap::new();
    for e in std::fs::read_dir("/proc").ok()?.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        if let Some((row, n)) = read_proc(pid) {
            fds.insert(pid, n);
            rows.push(row);
        }
    }
    let tree = tree_of(root, &rows);
    if !tree.iter().any(|r| r.pid == root) {
        return None;
    }
    Some(Snapshot {
        cpu_secs: tree.iter().map(|r| r.cpu_secs).sum(),
        rss_kb: tree.iter().map(|r| r.rss_kb).sum(),
        open_fds: Some(tree.iter().map(|r| fds.get(&r.pid).copied().unwrap_or(0)).sum()),
        procs: tree.len() as u64,
    })
}

/// Reads the tree under `root` (`ps`, and `lsof` for descriptors).
#[cfg(not(target_os = "linux"))]
pub fn snapshot(root: u32) -> Option<Snapshot> {
    let out = std::process::Command::new("ps")
        .args(["-Ao", "pid=,ppid=,cputime=,rss="])
        .output()
        .ok()?;
    let rows = parse_ps(&String::from_utf8_lossy(&out.stdout));
    let tree = tree_of(root, &rows);
    if !tree.iter().any(|r| r.pid == root) {
        return None;
    }
    let pids: Vec<String> = tree.iter().map(|r| r.pid.to_string()).collect();
    let open_fds = std::process::Command::new("lsof")
        .args(["-n", "-p", &pids.join(",")])
        .output()
        .ok()
        .filter(|o| !o.stdout.is_empty())
        .map(|o| (String::from_utf8_lossy(&o.stdout).lines().count() as u64).saturating_sub(1));
    Some(Snapshot {
        cpu_secs: tree.iter().map(|r| r.cpu_secs).sum(),
        rss_kb: tree.iter().map(|r| r.rss_kb).sum(),
        open_fds,
        procs: tree.len() as u64,
    })
}

/// Bytes `dir` occupies on disk (allocated blocks on Unix, so sparse and
/// pre-allocated files read as the disk sees them), 0 when it is missing.
pub fn dir_bytes(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                stack.push(e.path());
            } else if md.is_file() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    total += md.blocks() * 512;
                }
                #[cfg(not(unix))]
                {
                    total += md.len();
                }
            }
        }
    }
    total
}

/// A sampler running in the background until [`Sampler::finish`].
pub struct Sampler {
    stop: Arc<AtomicBool>,
    handle: tokio::task::JoinHandle<Vec<Sample>>,
    started_disk: u64,
}

impl Sampler {
    /// Samples the tree under `pid` and the size of `data_dir` every
    /// `interval`, starting now.
    pub fn start(pid: u32, data_dir: Option<PathBuf>, interval: Duration) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let started_disk = data_dir.as_deref().map_or(0, dir_bytes);
        let handle = tokio::spawn(async move {
            let mut out = Vec::new();
            let mut prev = tokio::task::spawn_blocking(move || snapshot(pid)).await.ok().flatten();
            let mut prev_at = Instant::now();
            while !flag.load(Ordering::Relaxed) {
                tokio::time::sleep(interval).await;
                let dd = data_dir.clone();
                let Ok((snap, disk)) =
                    tokio::task::spawn_blocking(move || (snapshot(pid), dd.as_deref().map_or(0, dir_bytes))).await
                else {
                    break;
                };
                let now = Instant::now();
                if let (Some(a), Some(b)) = (&prev, &snap) {
                    let dt = now.duration_since(prev_at).as_secs_f64().max(1e-3);
                    out.push(Sample {
                        cpu_pct: ((b.cpu_secs - a.cpu_secs) / dt * 100.0).max(0.0),
                        rss_kb: b.rss_kb,
                        open_fds: b.open_fds,
                        procs: b.procs,
                        disk_bytes: disk,
                    });
                }
                prev = snap;
                prev_at = now;
            }
            out
        });
        Self {
            stop,
            handle,
            started_disk,
        }
    }

    /// Stops sampling and summarises the samples.
    pub async fn finish(self) -> Summary {
        self.stop.store(true, Ordering::Relaxed);
        let samples = self.handle.await.unwrap_or_default();
        Summary::of(&samples, self.started_disk)
    }
}

/// p50 and max of a series.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct P50Max {
    /// Median.
    pub p50: f64,
    /// Maximum.
    pub max: f64,
}

fn p50_max(mut v: Vec<f64>) -> P50Max {
    if v.is_empty() {
        return P50Max::default();
    }
    v.sort_by(f64::total_cmp);
    P50Max {
        p50: v[(v.len() - 1) / 2],
        max: v[v.len() - 1],
    }
}

/// What a scenario cost the server.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Summary {
    /// Samples taken.
    pub samples: usize,
    /// CPU, percent of one core.
    pub cpu_pct: P50Max,
    /// Resident memory, MiB.
    pub rss_mb: P50Max,
    /// Open descriptors (`None`: not available).
    pub open_fds: Option<P50Max>,
    /// Data directory disk usage, MiB: p50 and max over the run.
    pub disk_mb: P50Max,
    /// Data directory usage before the scenario, MiB.
    pub disk_start_mb: f64,
    /// Data directory usage at the last sample, MiB.
    pub disk_end_mb: f64,
    /// Most processes seen in the tree.
    pub procs_max: u64,
}

impl Summary {
    /// Summarises `samples`.
    pub fn of(samples: &[Sample], disk_start: u64) -> Self {
        let mib = |b: u64| b as f64 / 1_048_576.0;
        let fds: Vec<f64> = samples.iter().filter_map(|s| s.open_fds).map(|n| n as f64).collect();
        Self {
            samples: samples.len(),
            cpu_pct: p50_max(samples.iter().map(|s| s.cpu_pct).collect()),
            rss_mb: p50_max(samples.iter().map(|s| s.rss_kb as f64 / 1024.0).collect()),
            open_fds: (!fds.is_empty()).then(|| p50_max(fds)),
            disk_mb: p50_max(samples.iter().map(|s| mib(s.disk_bytes)).collect()),
            disk_start_mb: mib(disk_start),
            disk_end_mb: samples.last().map_or(mib(disk_start), |s| mib(s.disk_bytes)),
            procs_max: samples.iter().map(|s| s.procs).max().unwrap_or(0),
        }
    }

    /// The summary as the report's JSON object.
    pub fn to_json(&self, scenario: &str) -> Value {
        let pm = |p: P50Max| json!({"p50": round(p.p50), "max": round(p.max)});
        json!({
            "scenario": scenario,
            "samples": self.samples,
            "cpu_pct": pm(self.cpu_pct),
            "rss_mb": pm(self.rss_mb),
            "open_fds": self.open_fds.map(pm),
            "disk_mb": {
                "start": round(self.disk_start_mb),
                "p50": round(self.disk_mb.p50),
                "max": round(self.disk_mb.max),
                "end": round(self.disk_end_mb),
            },
            "procs_max": self.procs_max,
        })
    }

    /// One human line for the console.
    pub fn line(&self, scenario: &str) -> String {
        format!(
            "RESOURCES scenario={scenario} samples={} cpu_pct_p50={:.0} cpu_pct_max={:.0} rss_mb_p50={:.0} rss_mb_max={:.0} \
             open_fds_p50={} open_fds_max={} disk_mb_start={:.1} disk_mb_end={:.1} disk_mb_max={:.1} procs_max={}",
            self.samples,
            self.cpu_pct.p50,
            self.cpu_pct.max,
            self.rss_mb.p50,
            self.rss_mb.max,
            self.open_fds.map_or("na".into(), |f| format!("{:.0}", f.p50)),
            self.open_fds.map_or("na".into(), |f| format!("{:.0}", f.max)),
            self.disk_start_mb,
            self.disk_end_mb,
            self.disk_mb.max,
            self.procs_max,
        )
    }
}

fn round(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_time_formats() {
        assert_eq!(parse_cputime("0:01.50"), Some(1.5));
        assert_eq!(parse_cputime("01:02:03"), Some(3723.0));
        assert_eq!(parse_cputime("2-00:00:10"), Some(172_810.0));
        assert_eq!(parse_cputime("x"), None);
    }

    #[test]
    fn ps_rows_and_trees() {
        let rows = parse_ps(
            "  10     1  0:01.00  1000\n  11    10  0:02.00  2000\n  12    11  0:00.50   500\n  99     1  9:00.00  9999\n",
        );
        assert_eq!(rows.len(), 4);
        let t = tree_of(10, &rows);
        assert_eq!(t.iter().map(|r| r.pid).collect::<Vec<_>>(), vec![10, 11, 12]);
        assert!((t.iter().map(|r| r.cpu_secs).sum::<f64>() - 3.5).abs() < 1e-9);
    }

    #[test]
    fn summary_p50_and_max() {
        let s = |cpu, rss, disk| Sample {
            cpu_pct: cpu,
            rss_kb: rss,
            open_fds: Some(10),
            procs: 3,
            disk_bytes: disk,
        };
        let sum = Summary::of(
            &[s(10.0, 1024, 1 << 20), s(30.0, 2048, 3 << 20), s(20.0, 1024, 2 << 20)],
            0,
        );
        assert_eq!(sum.samples, 3);
        assert_eq!(sum.cpu_pct, P50Max { p50: 20.0, max: 30.0 });
        assert_eq!(sum.rss_mb.max, 2.0);
        assert_eq!(sum.disk_end_mb, 2.0);
        assert_eq!(sum.open_fds.unwrap().max, 10.0);
        assert_eq!(Summary::of(&[], 0).samples, 0);
    }

    #[test]
    fn this_process_is_visible() {
        let snap = snapshot(std::process::id()).expect("own process");
        assert!(snap.rss_kb > 0 && snap.procs >= 1);
    }

    #[test]
    fn data_dir_bytes_counts_files() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a"), vec![1_u8; 10_000]).unwrap();
        assert!(dir_bytes(d.path()) >= 10_000);
        assert_eq!(dir_bytes(&d.path().join("missing")), 0);
    }
}
