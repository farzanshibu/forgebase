import type { CurrencyOption, Price, Tier } from "../../api";
import { formatMoney, parseMinor } from "./money";

/** `EUR=9.00` per line -> currency options. Amounts are decimal text in the currency, stored as minor units. */
export function parseCurrencyOptions(text: string): Record<string, CurrencyOption> {
  const out: Record<string, CurrencyOption> = {};
  for (const [i, raw] of text.split("\n").entries()) {
    const line = raw.trim();
    if (!line) continue;
    const m = /^([A-Za-z]{3})\s*=\s*(.+)$/.exec(line);
    if (!m) throw new Error(`Currency line ${i + 1} must look like EUR=9.00`);
    const cur = m[1]!.toUpperCase();
    const amt = parseMinor(m[2]!, cur);
    if (amt === null) throw new Error(`Currency line ${i + 1}: ${m[2]} is not a valid ${cur} amount`);
    out[cur] = { unit_amount: amt };
  }
  return out;
}

/** `up_to, unit, flat` per line (up_to `inf` for the last tier) -> tiers in minor units of `currency`. */
export function parseTiers(text: string, currency: string): Tier[] {
  const tiers: Tier[] = [];
  for (const [i, raw] of text.split("\n").entries()) {
    const line = raw.trim();
    if (!line) continue;
    const [upTo = "", unit = "0", flat = "0"] = line.split(",").map((s) => s.trim());
    const u = parseMinor(unit, currency);
    const f = parseMinor(flat, currency);
    const cap = /^(inf|)$/i.test(upTo) ? null : Number(upTo);
    if (u === null || f === null || (cap !== null && !Number.isInteger(cap))) throw new Error(`Tier line ${i + 1} must look like 100, 5.00, 0`);
    tiers.push({ up_to: cap, unit_amount: u, flat_amount: f });
  }
  return tiers;
}

export function describePrice(p: Price): string {
  const every = p.interval ? ` / ${p.interval_count > 1 ? `${p.interval_count} ` : ""}${p.interval}` : "";
  if (p.tiers && p.tiers.length > 0) return `${p.scheme} tiers (${p.tiers.length})${every}`;
  return `${formatMoney(p.unit_amount, p.currency)}${every}`;
}
