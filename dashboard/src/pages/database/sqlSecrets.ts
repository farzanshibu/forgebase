/**
 * What the SQL editor does when the server refuses a statement that touches secret columns (403, FB-143 and FB-153).
 * Two words, two answers: "set show_secrets" (SQLite and the PostgreSQL guard) is fixed by ticking Show secrets;
 * "permission denied" (PostgreSQL: the editor's role has no SELECT on them) cannot be fixed that way, so the only
 * way forward is to leave the secret columns out, which `expandStar` does for a `SELECT *`.
 */
import { ApiError } from "../../api/client";
import type { TableInfo } from "../../api";

/** Columns ForgeBase always treats as secret; password fields of a collection are added from the refusal's message. */
const ALWAYS_SECRET = ["password", "token_key"];

export interface SecretRefusal {
  /** The refusal names `show_secrets`: ticking it and running again helps. */
  canShowSecrets: boolean;
  /** The secret column the server named, when it did. */
  column?: string;
  message: string;
}

/** A 403 about secret columns, or undefined for any other error. */
export function secretRefusal(error: unknown): SecretRefusal | undefined {
  if (!(error instanceof ApiError) || error.status !== 403) return undefined;
  const m = error.message;
  const column = /secret column (\w+)/i.exec(m)?.[1];
  const canShowSecrets = /show_secrets/.test(m);
  if (!column && !canShowSecrets && !/permission denied/i.test(m)) return undefined;
  return { canShowSecrets, column, message: m };
}

const STAR = /^\s*select\s+\*\s+from\s+("?)([A-Za-z_][\w.]*)\1((?:\s[\s\S]*)?);?\s*$/i;

/** `SELECT * FROM t …` with the secret columns left out, or undefined when `sql` is not that shape or `t` is unknown. */
export function expandStar(sql: string, tables: readonly TableInfo[], hide: readonly string[] = []): string | undefined {
  const m = STAR.exec(sql.replace(/;\s*$/, ""));
  if (!m) return undefined;
  const table = tables.find((t) => t.name.toLowerCase() === m[2]!.toLowerCase());
  if (!table) return undefined;
  const secret = new Set([...ALWAYS_SECRET, ...hide].map((c) => c.toLowerCase()));
  const cols = table.columns.map((c) => c.name).filter((c) => !secret.has(c.toLowerCase()));
  if (cols.length === 0 || cols.length === table.columns.length) return undefined;
  const q = (id: string) => `"${id.replace(/"/g, '""')}"`;
  return `SELECT ${cols.map(q).join(", ")} FROM ${q(table.name)}${m[3] ?? ""}`;
}
