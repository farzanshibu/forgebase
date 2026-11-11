/** Types and helpers behind ResourceRules: the policy-engine routes and the mapping between stored rules and the editor's rows. */
import { get, send } from "../api/client";

export type RuleKind = "kv_namespace" | "vector_collection" | "graph_label" | "graph_edge_type" | "bucket" | "queue_topic" | "queue_subscription";
export type RuleOp = "list" | "view" | "create" | "update" | "delete";
export type RuleSet = Record<string, string | null>;

export interface RuleKindInfo {
  kind: string;
  fields: { name: string; type: string }[];
  operations: string[];
  actions: string[];
  items: { name: string; rules: RuleSet }[];
}

const R = "/api/v1/policies/resources";
const enc = encodeURIComponent;
export const getRuleKind = (kind: string, signal?: AbortSignal) => get<RuleKindInfo>(`${R}/${enc(kind)}`, signal);
export const putResourceRules = (kind: string, name: string, rules: RuleSet) => send<unknown>("PUT", `${R}/${enc(kind)}/${enc(name)}`, { rules });
export const deleteResourceRules = (kind: string, name: string) => send<void>("DELETE", `${R}/${enc(kind)}/${enc(name)}`);

export const OP_TEXT: Record<RuleOp, string> = {
  list: "List (scan or search)",
  view: "View (read one)",
  create: "Create",
  update: "Update",
  delete: "Delete",
};

export type Mode = "locked" | "everyone" | "expr";
export interface Row { mode: Mode; expr: string }

/** The stored `read` and `write` aliases cover several operations; an explicit operation wins. */
export function expand(stored: RuleSet): Record<RuleOp, string | null | undefined> {
  const out: Record<RuleOp, string | null | undefined> = { list: undefined, view: undefined, create: undefined, update: undefined, delete: undefined };
  const groups: Record<string, RuleOp[]> = { read: ["list", "view"], write: ["create", "update", "delete"] };
  for (const [k, ops] of Object.entries(groups)) if (k in stored) for (const o of ops) out[o] = stored[k];
  for (const o of Object.keys(out) as RuleOp[]) if (o in stored) out[o] = stored[o];
  return out;
}

export const toRow = (v: string | null | undefined): Row => (v === undefined || v === null ? { mode: "locked", expr: "" } : v === "" ? { mode: "everyone", expr: "" } : { mode: "expr", expr: v });

/** The body for PUT: only operations that are not locked (a locked operation has no rule). */
export function toRules(rows: Partial<Record<RuleOp, Row>>): RuleSet {
  const out: RuleSet = {};
  for (const [op, r] of Object.entries(rows) as [RuleOp, Row][]) {
    if (r.mode === "everyone") out[op] = "";
    else if (r.mode === "expr") out[op] = r.expr.trim();
  }
  return out;
}

