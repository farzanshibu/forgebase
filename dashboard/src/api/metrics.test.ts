import { describe, expect, it } from "vitest";
import { bucketDelta, histogramBuckets, histogramQuantile, parsePrometheus, pointFrom, pushHistory, ratePerSec, readingFrom, sumOf } from "./metrics";

const TEXT = `# HELP forgebase_http_requests_total HTTP requests served.
# TYPE forgebase_http_requests_total counter
forgebase_http_requests_total{method="GET",route="/api/v1/x",status="200"} 90
forgebase_http_requests_total{method="GET",route="/a \\"q\\"",status="500"} 10
forgebase_http_requests_in_flight 2
forgebase_realtime_connections 3 1700000000
forgebase_bad{oops 1
forgebase_nan NaN
forgebase_http_request_duration_seconds_bucket{route="/a",le="0.01"} 50
forgebase_http_request_duration_seconds_bucket{route="/a",le="0.1"} 80
forgebase_http_request_duration_seconds_bucket{route="/b",le="0.01"} 0
forgebase_http_request_duration_seconds_bucket{route="/b",le="0.1"} 20
forgebase_http_request_duration_seconds_bucket{route="/a",le="+Inf"} 80
forgebase_http_request_duration_seconds_bucket{route="/b",le="+Inf"} 20
`;

describe("parsePrometheus", () => {
  const s = parsePrometheus(TEXT);
  it("reads names, labels (with escapes), values and ignores comments, bad lines, NaN, timestamps", () => {
    expect(s.find((x) => x.name === "forgebase_http_requests_in_flight")?.value).toBe(2);
    expect(s.find((x) => x.labels.route === '/a "q"')?.labels.status).toBe("500");
    expect(s.find((x) => x.name === "forgebase_realtime_connections")?.value).toBe(3);
    expect(s.some((x) => x.name === "forgebase_bad" || x.name === "forgebase_nan")).toBe(false);
  });
  it("sumOf is undefined for an absent metric, never 0", () => {
    expect(sumOf(s, "forgebase_http_requests_total")).toBe(100);
    expect(sumOf(s, "forgebase_http_requests_total", (l) => l.status === "500")).toBe(10);
    expect(sumOf(s, "process_cpu_seconds_total")).toBeUndefined();
  });
});

describe("histograms", () => {
  const b = histogramBuckets(parsePrometheus(TEXT), "forgebase_http_request_duration_seconds");
  it("sums label sets per bucket, sorted", () => {
    expect(b).toEqual([[0.01, 50], [0.1, 100], [Infinity, 100]]);
  });
  it("interpolates quantiles like histogram_quantile", () => {
    expect(histogramQuantile(0.5, b)).toBeCloseTo(0.01, 5);
    expect(histogramQuantile(0.75, b)).toBeCloseTo(0.01 + 0.09 * 0.5, 5);
    expect(histogramQuantile(0.99, b)).toBeCloseTo(0.01 + 0.09 * (49 / 50), 5);
  });
  it("is undefined with no observations and clamps to the last finite bound above it", () => {
    expect(histogramQuantile(0.5, [])).toBeUndefined();
    expect(histogramQuantile(0.5, [[0.1, 0], [Infinity, 0]])).toBeUndefined();
    expect(histogramQuantile(0.99, [[0.1, 1], [Infinity, 10]])).toBe(0.1);
  });
  it("bucketDelta subtracts and treats a counter reset as a fresh start", () => {
    expect(bucketDelta([[1, 5], [Infinity, 9]], [[1, 2], [Infinity, 4]])).toEqual([[1, 3], [Infinity, 5]]);
    expect(bucketDelta([[1, 1], [Infinity, 2]], [[1, 7], [Infinity, 9]])).toEqual([[1, 1], [Infinity, 2]]);
  });
});

describe("rates and points", () => {
  it("ratePerSec", () => {
    expect(ratePerSec(10, 30, 5000)).toBe(4);
    expect(ratePerSec(undefined, 30, 5000)).toBeUndefined();
    expect(ratePerSec(30, 10, 5000)).toBeUndefined();
    expect(ratePerSec(1, 2, 0)).toBeUndefined();
  });
  it("derives rps, error share, latency and cpu from two readings; absent metrics stay undefined", () => {
    const a = readingFrom(parsePrometheus(TEXT), 0);
    const more = TEXT.replace("} 90", "} 190").replace("} 10\n", "} 20\n").replace("rt_connections 3", "rt_connections 3");
    const b = readingFrom(parsePrometheus(more), 10_000);
    const p = pointFrom(a, b);
    expect(p.rps).toBe(11);
    expect(p.errorRate).toBeCloseTo(10 / 110, 5);
    expect(p.cpuPct).toBeUndefined();
    expect(p.rssBytes).toBeUndefined();
    expect(p.p95).toBeDefined();
    const first = pointFrom(undefined, a);
    expect(first.rps).toBeUndefined();
    expect(first.p50).toBeDefined();
  });
  it("cpu percent from process_cpu_seconds_total", () => {
    const a = readingFrom(parsePrometheus("process_cpu_seconds_total 1\n"), 0);
    const b = readingFrom(parsePrometheus("process_cpu_seconds_total 2\n"), 4000);
    expect(pointFrom(a, b).cpuPct).toBe(25);
  });
  it("pushHistory caps the length", () => {
    let h: { t: number }[] = [];
    for (let i = 0; i < 5; i++) h = pushHistory(h, { t: i }, 3);
    expect(h.map((x) => x.t)).toEqual([2, 3, 4]);
  });
});
