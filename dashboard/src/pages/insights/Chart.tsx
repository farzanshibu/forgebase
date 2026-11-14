import { useId } from "react";

export interface ChartSeries { name: string; values: (number | null)[]; className?: string }

/**
 * A small inline-SVG line chart over equally spaced points, with a caption that says what it shows (so it is read,
 * not only seen). Null values leave gaps. Each series shares the vertical scale; the first is drawn as an area.
 *
 * Usage:
 *   <Chart label="p95 latency" unit="ms" labels={days} series={[{ name: "p95", values: p95 }]} format={fmtMs} />
 */
export default function Chart({ label, labels, series, format = (n) => String(Math.round(n * 100) / 100) }: {
  label: string;
  labels: string[];
  series: ChartSeries[];
  format?: (n: number) => string;
}) {
  const id = useId();
  const w = 400;
  const h = 90;
  const all = series.flatMap((s) => s.values).filter((v): v is number => v !== null);
  if (labels.length < 2 || all.length === 0) {
    return <p className="rounded-lg border border-dashed border-border p-3 text-sm text-muted-foreground">{label}: not enough points yet. Statistics are written every flush interval.</p>;
  }
  const max = Math.max(...all, 1e-9);
  const step = w / (labels.length - 1);
  const y = (v: number) => h - 2 - (v / max) * (h - 6);
  const runs = (values: (number | null)[]) => {
    const out: [number, number][][] = [];
    let cur: [number, number][] = [];
    values.forEach((v, i) => {
      if (v === null) {
        if (cur.length) out.push(cur);
        cur = [];
      } else cur.push([i * step, y(v)]);
    });
    if (cur.length) out.push(cur);
    return out;
  };
  const line = (r: [number, number][]) => r.map(([x, yy], i) => `${i ? "L" : "M"}${x.toFixed(1)},${yy.toFixed(1)}`).join("");
  const summary = series.map((s) => {
    const v = s.values.filter((x): x is number => x !== null);
    return `${s.name}: latest ${v.length ? format(v[v.length - 1]!) : "none"}, peak ${v.length ? format(Math.max(...v)) : "none"}`;
  }).join("; ");
  return (
    <figure className="space-y-1">
      <figcaption id={`${id}-cap`} className="flex flex-wrap items-baseline justify-between gap-2 text-xs text-muted-foreground">
        <span className="font-medium text-foreground">{label}</span>
        <span>{labels[0]} to {labels[labels.length - 1]} · peak {format(max)}</span>
      </figcaption>
      <svg role="img" aria-label={`${label}. ${summary}`} viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none" className="h-24 w-full rounded-lg border border-border bg-card">
        {series.map((s, si) => (
          <g key={s.name} className={s.className ?? (si === 0 ? "text-primary" : "text-warning")}>
            {si === 0 && runs(s.values).map((r, i) => <path key={`a${i}`} d={`${line(r)}L${r[r.length - 1]![0].toFixed(1)},${h}L${r[0]![0].toFixed(1)},${h}Z`} fill="currentColor" opacity="0.1" />)}
            {runs(s.values).map((r, i) => <path key={i} d={line(r)} fill="none" stroke="currentColor" strokeWidth="1.75" strokeLinejoin="round" vectorEffect="non-scaling-stroke" />)}
          </g>
        ))}
      </svg>
      {series.length > 1 && (
        <ul className="flex gap-3 text-xs text-muted-foreground" aria-label="Legend">
          {series.map((s, si) => <li key={s.name} className="flex items-center gap-1"><span aria-hidden="true" className={`inline-block h-0.5 w-4 bg-current ${s.className ?? (si === 0 ? "text-primary" : "text-warning")}`} />{s.name}</li>)}
        </ul>
      )}
    </figure>
  );
}
