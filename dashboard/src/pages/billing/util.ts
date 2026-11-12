/** Billing's form grid and small helpers. Times in tables are `When` (components/Time); `when_` writes one in a sentence. */
export const FORM_GRID = "grid gap-3 sm:grid-cols-2";

const DAY_TIME = new Intl.DateTimeFormat("en", { dateStyle: "medium", timeStyle: "short" });

function fmt(f: Intl.DateTimeFormat, iso: string | null | undefined, empty: string): string {
  if (!iso) return empty;
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : f.format(d);
}

/** A moment: "Nov 10, 2026, 14:03". */
export const when_ = (iso: string | null | undefined) => fmt(DAY_TIME, iso, "never");

export const idempotencyKey = (prefix: string) => `${prefix}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;

/** Plain words for the API's codes in selects and cells: `one_time` reads "one time". */
export const words = (code: string) => code.replace(/_/g, " ");
