#!/usr/bin/env python3
"""Collect, compare and refresh ForgeBase benchmark results (stdlib only).

  bench_report.py criterion --dir target/criterion --backend postgres --out criterion-postgres.json
  bench_report.py compare   --baseline baseline/linux.json RUN.json [RUN.json ...] [--threshold 20]
  bench_report.py baseline  --out baseline/linux.json --runner "ubuntu-24.04 ..." RUN.json [RUN.json ...]

RUN.json is either a criterion summary (`criterion` above) or an fb-load report (`fb-load --json`,
format 1, crates/forgebase-bench/src/report.rs). `compare` never fails the build: it prints a table,
GitHub `::warning` annotations for every metric more than --threshold percent worse than the
baseline, and appends the table to $GITHUB_STEP_SUMMARY when set. A baseline value of null means
"not recorded yet": the metric is listed as unbaselined and never warns.

Baseline layout (baseline/linux.json), all numbers null until a Linux run is recorded:
  {"format": 1, "note": ..., "runner": ..., "commit": ..., "recorded_at": ...,
   "criterion": {<backend>: {"<group>/<bench>": {"median_ns": N}}},
   "load":      {<backend>: {"<scenario>/<op>": {"rps": N, "p95_us": N, "p99_us": N}}},
   "resources": {<backend>: {"<scenario>": {"rss_mb_max": N, "cpu_pct_p50": N}}}}
"""
import argparse
import json
import os
import sys
from pathlib import Path

FORMAT = 1
# metric kind -> True when a higher value is better
HIGHER_IS_BETTER = {"rps": True, "median_ns": False, "p95_us": False, "p99_us": False,
                    "rss_mb_max": False, "cpu_pct_p50": False}


def collect_criterion(directory: Path, backend: str) -> dict:
    """Reads criterion's `<dir>/**/new/{benchmark,estimates}.json` pairs."""
    benches = {}
    for est in sorted(directory.glob("**/new/estimates.json")):
        meta = est.with_name("benchmark.json")
        if not meta.exists():
            continue
        full_id = json.loads(meta.read_text()).get("full_id")
        median = json.loads(est.read_text()).get("median", {}).get("point_estimate")
        if full_id and median is not None:
            benches[full_id] = {"median_ns": round(median, 1)}
    return {"format": FORMAT, "kind": "criterion", "backend": backend, "benches": benches}


def metrics_of(run: dict) -> dict:
    """Flattens one run file into {(section, backend, key, kind): value}."""
    out = {}
    if run.get("kind") == "criterion":
        for bench, v in run.get("benches", {}).items():
            out[("criterion", run["backend"], bench, "median_ns")] = v["median_ns"]
    elif run.get("tool") == "fb-load":
        backend = run.get("backend", "unknown")
        for row in run.get("results", []):
            if row.get("errors", 0) or not row.get("n"):
                continue  # a row with errors is not a measurement
            key = f'{row["scenario"]}/{row["op"]}'
            for kind in ("rps", "p95_us", "p99_us"):
                if kind in row:
                    out[("load", backend, key, kind)] = row[kind]
        for res in run.get("resources", []):
            if res.get("samples"):
                out[("resources", backend, res["scenario"], "rss_mb_max")] = res["rss_mb"]["max"]
                out[("resources", backend, res["scenario"], "cpu_pct_p50")] = res["cpu_pct"]["p50"]
    else:
        raise SystemExit(f"unrecognised run file (neither criterion nor fb-load): {sorted(run)[:6]}")
    return out


def baseline_value(baseline: dict, section: str, backend: str, key: str, kind: str):
    return ((baseline.get(section, {}).get(backend, {}) or {}).get(key, {}) or {}).get(kind)


def noise_floor(kind: str, floor_us: float) -> float:
    """Smallest absolute change worth a warning, in the metric's unit."""
    return {"median_ns": floor_us * 1000, "p95_us": floor_us, "p99_us": floor_us}.get(kind, 0)


def compare(baseline: dict, runs: list, threshold: float, floor_us: float):
    rows, unbaselined = [], 0
    for run in runs:
        for (section, backend, key, kind), cur in sorted(metrics_of(run).items()):
            base = baseline_value(baseline, section, backend, key, kind)
            if base is None or base == 0:
                unbaselined += 1
                continue
            change = (cur - base) / base * 100.0
            worse = -change if HIGHER_IS_BETTER[kind] else change
            if abs(cur - base) < noise_floor(kind, floor_us):
                worse = 0.0
            rows.append((worse, section, backend, key, kind, base, cur, change))
    return rows, unbaselined


def fmt(x) -> str:
    return f"{x:,.1f}" if isinstance(x, float) and x < 1000 else f"{x:,.0f}"


def cmd_compare(a) -> int:
    baseline = json.loads(Path(a.baseline).read_text())
    runs = [json.loads(Path(p).read_text()) for p in a.runs]
    rows, unbaselined = compare(baseline, runs, a.threshold, a.noise_floor_us)
    regressions = sorted((r for r in rows if r[0] > a.threshold), reverse=True)
    better = sorted((r for r in rows if r[0] < -a.threshold))
    lines = [f"### Benchmark comparison against `{a.baseline}` (warn above {a.threshold:g} % worse)", ""]
    if baseline.get("recorded_at") is None and not rows:
        lines += [f"> The baseline has no recorded values yet ({baseline.get('note', 'TBD')}). "
                  f"{unbaselined} metrics were measured and none could be compared.", ""]
    lines.append(f"{len(rows)} metrics compared, {len(regressions)} regressed, {len(better)} improved, "
                 f"{unbaselined} without a baseline value.")
    if regressions or better:
        lines += ["", "| Metric | Baseline | Now | Change |", "|---|---|---|---|"]
        for worse, section, backend, key, kind, base, cur, change in regressions + better:
            flag = "REGRESSED" if worse > 0 else "improved"
            lines.append(f"| {section}/{backend}/{key} {kind} | {fmt(base)} | {fmt(cur)} | {change:+.1f} % {flag} |")
    text = "\n".join(lines)
    print(text)
    for worse, section, backend, key, kind, base, cur, change in regressions:
        print(f"::warning title=Benchmark regression::{section}/{backend}/{key} {kind}: "
              f"{fmt(base)} -> {fmt(cur)} ({change:+.1f} %)")
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a") as f:
            f.write(text + "\n")
    return 0  # warn, never fail


def cmd_baseline(a) -> int:
    base = {"format": FORMAT, "note": a.note, "runner": a.runner, "commit": a.commit,
            "recorded_at": a.recorded_at, "criterion": {}, "load": {}, "resources": {}}
    for p in a.runs:
        for (section, backend, key, kind), v in metrics_of(json.loads(Path(p).read_text())).items():
            base[section].setdefault(backend, {}).setdefault(key, {})[kind] = v
    Path(a.out).write_text(json.dumps(base, indent=2, sort_keys=True) + "\n")
    print(f"wrote {a.out}")
    return 0


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("criterion")
    c.add_argument("--dir", required=True)
    c.add_argument("--backend", required=True)
    c.add_argument("--out", required=True)
    k = sub.add_parser("compare")
    k.add_argument("--baseline", required=True)
    k.add_argument("--threshold", type=float, default=20.0)
    k.add_argument("--noise-floor-us", type=float, default=50.0,
                   help="ignore latency changes smaller than this many microseconds")
    k.add_argument("runs", nargs="+")
    b = sub.add_parser("baseline")
    b.add_argument("--out", required=True)
    b.add_argument("--note", default="Recorded from a Linux CI run (bench.yml).")
    b.add_argument("--runner", default=None)
    b.add_argument("--commit", default=None)
    b.add_argument("--recorded-at", dest="recorded_at", default=None)
    b.add_argument("runs", nargs="+")
    a = p.parse_args(argv)
    if a.cmd == "criterion":
        Path(a.out).write_text(json.dumps(collect_criterion(Path(a.dir), a.backend), indent=2) + "\n")
        return 0
    return {"compare": cmd_compare, "baseline": cmd_baseline}[a.cmd](a)


if __name__ == "__main__":
    sys.exit(main())
