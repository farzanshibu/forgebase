/**
 * Facts: the key facts of one resource, small label over its value, under a detail page's header. The same strip in
 * every studio. Items are `[label, value]` tuples or `{ label, value, mono?, title? }` objects; a falsy item is
 * skipped, so `cond && { … }` reads naturally.
 *
 * Usage:
 *   <Facts label="Function summary" items={[{ label: "Version", value: "v2 of 2" }, { label: "Id", value: f.id, mono: true }]} />
 *   <Facts label="Collection facts" items={[["Type", "base"], ["Fields", 6], hasRules && ["Rules", "on"]]} />
 *   <Facts label="Job details" columns={2} framed={false} items={…} />   // a grid inside a card that already has a border
 *
 * Without `columns` the facts share a row and wrap; with `columns` (1 to 4) they sit in a grid. Long values are cut
 * to fit in a row (pass `title` for the whole value) and wrap in a grid.
 */
import type { ReactNode } from "react";

export interface Fact {
  label: ReactNode;
  value: ReactNode;
  /** Monospace value: ids, versions, sizes. */
  mono?: boolean;
  /** Hover words, for a value that is cut short. */
  title?: string;
}

export type FactItem = Fact | readonly [ReactNode, ReactNode] | false | null | undefined | 0 | "";

const GRID: Record<number, string> = {
  1: "grid grid-cols-1",
  2: "grid grid-cols-1 sm:grid-cols-2",
  3: "grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3",
  4: "grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4",
};

const asFact = (item: Exclude<FactItem, false | null | undefined | 0 | "">): Fact => (Array.isArray(item) ? { label: item[0], value: item[1] } : (item as Fact));

export function Facts({ items, label, columns, framed = true, className = "" }: { items: readonly FactItem[]; label?: string; columns?: 1 | 2 | 3 | 4; framed?: boolean; className?: string }) {
  const shown = items.filter((x): x is Exclude<FactItem, false | null | undefined | 0 | ""> => !!x).map(asFact);
  const layout = columns ? `${GRID[columns]} gap-x-8 gap-y-3` : "flex flex-wrap gap-x-8 gap-y-3";
  const frame = framed ? "rounded-xl border border-border bg-card px-4 py-3" : "";
  return (
    <dl aria-label={label} className={`${layout} ${frame} text-sm ${className}`.replace(/\s+/g, " ").trim()}>
      {shown.map((f, i) => (
        <div key={i} className="min-w-0 max-w-full">
          <dt className="text-xs text-muted-foreground">{f.label}</dt>
          <dd title={f.title} className={`mt-0.5 font-medium text-foreground ${columns ? "break-words" : "truncate"} ${f.mono ? "font-mono text-[0.8125rem]" : ""}`.trim()}>
            {f.value}
          </dd>
        </div>
      ))}
    </dl>
  );
}
