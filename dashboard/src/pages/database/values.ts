/** Cell value helpers for the grid editor. */
import type { FieldDef } from "../../api";

export function display(v: unknown): string {
  if (v === null || v === undefined) return "";
  if (typeof v === "object") return JSON.stringify(v);
  return String(v);
}

/** Parse what the user typed into the JSON value for `field`; throws a readable Error. */
export function parseCell(field: FieldDef | undefined, text: string): unknown {
  const t = field?.type ?? "text";
  if (t === "number") {
    if (text.trim() === "") return null;
    const n = Number(text);
    if (Number.isNaN(n)) throw new Error("Not a number");
    return n;
  }
  if (t === "bool") {
    const s = text.trim().toLowerCase();
    if (s === "true" || s === "1") return true;
    if (s === "false" || s === "0") return false;
    throw new Error("Use true or false");
  }
  if (t === "json" || t === "array" || t === "geo" || t === "vector") {
    try {
      return JSON.parse(text) as unknown;
    } catch {
      throw new Error("Not valid JSON");
    }
  }
  return text;
}

export const READ_ONLY_KEYS = ["id", "created", "updated", "_version", "expand"];

export const PER_PAGE = 25;

/** The grid's state as it sits in the URL: `?filter=&sort=&page=&record=&search=&fuzzy=1&ft=0.4`. */
export function readGridState(p: URLSearchParams) {
  const page = Number.parseInt(p.get("page") ?? "1", 10);
  return {
    filter: p.get("filter") ?? "", sort: p.get("sort") ?? "", page: Number.isFinite(page) && page > 0 ? page : 1, record: p.get("record") ?? "",
    search: p.get("search") ?? "", fuzzy: p.get("fuzzy") === "1", threshold: p.get("ft") ?? "",
  };
}

/** The fuzzy threshold box: blank (server default 0.3) or a number from 0 to 1. */
export const fuzzyThresholdError = (t: string): string | undefined => {
  if (t.trim() === "") return undefined;
  const n = Number(t);
  return Number.isFinite(n) && n >= 0 && n <= 1 ? undefined : "A number from 0 to 1, such as 0.3.";
};

