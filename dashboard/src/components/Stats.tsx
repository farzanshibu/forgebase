/**
 * StatGrid and Stat: key figures as a row of small cards, label over a large value, the same in every studio. A `dl`,
 * so each label names its value. Pass `items` (data) or `Stat` children (markup); both draw the same card.
 *
 * Usage:
 *   <StatGrid label="Topic metrics" items={[{ label: "Retained", value: 3 }, { label: "Failed", value: 2, tone: "bad", hint: "last hour" }]} />
 *   <StatGrid label="Revenue summary" columns={4}>
 *     <Stat label="MRR" value={<Money amount={1299} currency="USD" />} />
 *     <Stat label="Failed payments" value={2} tone="bad" hint="see Dunning" onClick={() => setFilter("failed")} />
 *   </StatGrid>
 *
 * `tone` puts a dot before the label and colours the value (ok green, warn amber, bad red). `columns` (2 to 7) sets
 * the widest row; it defaults to the number of stats. With `onClick` the value is a button that filters the page.
 */
import { Children, type ReactNode } from "react";
import { Dot, type Tone } from "./StatusBadge";

export interface StatItem {
  label: ReactNode;
  value: ReactNode;
  /** A quieter line under the value. */
  hint?: ReactNode;
  tone?: Tone;
  /** Hover words for the value. */
  title?: string;
  onClick?: () => void;
}

const VALUE_TONE: Partial<Record<Tone, string>> = { bad: "text-destructive", warn: "text-warning", ok: "text-success" };

/** One figure: what it is, the value large, and a quiet line under it. Put several in a StatGrid. */
export function Stat({ label, value, hint, tone, title, onClick }: StatItem) {
  const color = (tone && VALUE_TONE[tone]) ?? "text-foreground";
  const name = typeof label === "string" && (typeof value === "string" || typeof value === "number") ? `${label}: ${value}` : undefined;
  return (
    <div className={`relative min-w-0 rounded-xl border border-border bg-card px-4 py-3 ${onClick ? "transition-colors hover:bg-muted/40" : ""}`.trim()}>
      <dt className="flex items-center gap-1.5 truncate text-xs font-medium text-muted-foreground">
        {tone && tone !== "default" && <Dot tone={tone} />}
        {label}
      </dt>
      <dd className={`mt-1 truncate text-xl font-semibold tracking-tight tabular-nums ${color}`} title={title}>
        {onClick ? (
          <button type="button" onClick={onClick} aria-label={name} className="rounded-sm text-left after:absolute after:inset-0 after:rounded-xl focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring">
            {value}
          </button>
        ) : (
          value
        )}
      </dd>
      {hint && <dd className="mt-0.5 truncate text-xs text-muted-foreground">{hint}</dd>}
    </div>
  );
}

const COLS: Record<number, string> = {
  1: "grid-cols-1",
  2: "grid-cols-2",
  3: "grid-cols-2 sm:grid-cols-3",
  4: "grid-cols-2 lg:grid-cols-4",
  5: "grid-cols-2 sm:grid-cols-3 lg:grid-cols-5",
  6: "grid-cols-2 sm:grid-cols-3 lg:grid-cols-6",
  7: "grid-cols-2 sm:grid-cols-4 xl:grid-cols-7",
};

/** Stat cards in a responsive grid. */
export function StatGrid({ label, items, children, columns, className = "" }: { label: string; items?: StatItem[]; children?: ReactNode; columns?: number; className?: string }) {
  const count = items ? items.length : Children.toArray(children).length;
  const cols = COLS[Math.max(1, Math.min(columns ?? count, 7))] ?? COLS[6];
  return (
    <dl aria-label={label} className={`grid gap-3 ${cols} ${className}`.trim()}>
      {items?.map((i, n) => <Stat key={n} {...i} />)}
      {children}
    </dl>
  );
}
