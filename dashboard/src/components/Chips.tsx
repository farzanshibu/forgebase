/**
 * Chips: short words as small rounded tags, such as roles, scopes or a kind. Not a state (use Badge for that) and not a
 * control. `max` folds the rest into "+3 more" with the full list on hover.
 *
 * Usage:
 *   <Chips items={user.roles} max={3} />                        // mono chips, "—" when empty
 *   <Chips items={scopes} tone={(s) => (s === "*" ? "warn" : undefined)} />
 *   <Chip>orders:read</Chip>   <Chip mono={false}>recurring</Chip>
 */
import type { ReactNode } from "react";
import type { Tone } from "./statusTone";

const TONE: Partial<Record<Tone, string>> = {
  bad: "border-destructive/30 bg-destructive/10 text-destructive",
  ok: "border-success/30 bg-success/10 text-success",
  warn: "border-warning/30 bg-warning/10 text-warning",
  info: "border-info/30 bg-info/10 text-info",
};

/** One chip. `mono` (default) for codes such as scopes and role keys. */
export function Chip({ children, mono = true, tone, title }: { children: ReactNode; mono?: boolean; tone?: Tone; title?: string }) {
  const c = (tone && TONE[tone]) ?? "border-border bg-muted/60 text-foreground/80";
  const Tag = mono ? "code" : "span";
  return (
    <Tag title={title} className={`inline-flex h-5 max-w-full items-center truncate whitespace-nowrap rounded-md border px-1.5 text-[0.6875rem] leading-none ${mono ? "font-mono" : "font-sans"} ${c}`}>
      {children}
    </Tag>
  );
}

/** Words as chips in a wrapping row; `empty` shows for no items. */
export function Chips({ items, max = items.length, mono = true, empty = "—", tone }: { items: readonly string[]; max?: number; mono?: boolean; empty?: ReactNode; tone?: (item: string) => Tone | undefined }) {
  if (items.length === 0) return <span className="text-muted-foreground">{empty}</span>;
  const shown = items.slice(0, max);
  const rest = items.slice(max);
  return (
    <span className="flex flex-wrap items-center gap-1">
      {shown.map((s) => (
        <Chip key={s} mono={mono} tone={tone?.(s)}>
          {s}
        </Chip>
      ))}
      {rest.length > 0 && (
        <span title={rest.join("\n")} className="inline-flex h-5 items-center rounded-md px-1 text-xs text-muted-foreground">
          +{rest.length} more
        </span>
      )}
    </span>
  );
}
