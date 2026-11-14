"""Self-test of bench_report.py: python3 -m unittest crates/forgebase-bench/scripts/test_bench_report.py"""
import contextlib
import io
import json
import tempfile
import unittest
from pathlib import Path

import bench_report as br

LOAD = {"format": 1, "tool": "fb-load", "backend": "postgres",
        "results": [{"scenario": "records", "op": "create", "n": 100, "errors": 0, "rps": 700, "p95_us": 5000, "p99_us": 9000},
                    {"scenario": "records", "op": "bad", "n": 10, "errors": 2, "rps": 1, "p95_us": 1, "p99_us": 1}],
        "resources": [{"scenario": "records", "samples": 5, "rss_mb": {"p50": 90, "max": 120}, "cpu_pct": {"p50": 200, "max": 300}}]}
CRIT = {"format": 1, "kind": "criterion", "backend": "postgres", "benches": {"db/point": {"median_ns": 1300000.0}}}
BASE = {"format": 1, "recorded_at": "2026-12-01", "criterion": {"postgres": {"db/point": {"median_ns": 1000000}}},
        "load": {"postgres": {"records/create": {"rps": 1000, "p95_us": 5000, "p99_us": None}}}, "resources": {}}


class Compare(unittest.TestCase):
    def test_metrics_skip_rows_with_errors(self):
        m = br.metrics_of(LOAD)
        self.assertIn(("load", "postgres", "records/create", "rps"), m)
        self.assertNotIn(("load", "postgres", "records/bad", "rps"), m)
        self.assertEqual(m[("resources", "postgres", "records", "rss_mb_max")], 120)

    def test_regressions_and_missing_baseline(self):
        rows, unbaselined = br.compare(BASE, [LOAD, CRIT], 20.0, 50.0)
        worse = {(r[3], r[4]): round(r[0]) for r in rows}
        self.assertEqual(worse[("records/create", "rps")], 30)      # 1000 -> 700: 30 % worse
        self.assertEqual(worse[("db/point", "median_ns")], 30)      # 1 ms -> 1.3 ms: 30 % worse
        self.assertEqual(worse[("records/create", "p95_us")], 0)    # unchanged
        self.assertGreater(unbaselined, 0)                           # p99 null, resources absent

    def test_noise_floor(self):
        base = {"criterion": {"postgres": {"db/point": {"median_ns": 100.0}}}}
        run = {"kind": "criterion", "backend": "postgres", "benches": {"db/point": {"median_ns": 300.0}}}
        rows, _ = br.compare(base, [run], 20.0, 50.0)
        self.assertEqual(rows[0][0], 0.0)  # +200 ns is under the 50 us floor

    def test_compare_exits_zero_and_warns(self):
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            (d / "b.json").write_text(json.dumps(BASE))
            (d / "l.json").write_text(json.dumps(LOAD))
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                code = br.main(["compare", "--baseline", str(d / "b.json"), str(d / "l.json")])
            self.assertEqual(code, 0)
            self.assertIn("::warning title=Benchmark regression::load/postgres/records/create rps", out.getvalue())

    def test_baseline_round_trip(self):
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            (d / "l.json").write_text(json.dumps(LOAD))
            br.main(["baseline", "--out", str(d / "o.json"), str(d / "l.json")])
            got = json.loads((d / "o.json").read_text())
            self.assertEqual(got["load"]["postgres"]["records/create"]["rps"], 700)


if __name__ == "__main__":
    unittest.main()
