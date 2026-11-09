import type { Price, Subscription } from "../../api";
import { prorate } from "./money";

/**
 * Proration preview. The server has no preview endpoint (reported), so this is an estimate from the current period
 * and the two prices' amounts in the subscription's currency: unused credit of the old price against the new price's remainder.
 */
export function prorationPreview(sub: Subscription, current: Price | undefined, next: Price | undefined, now = Date.now()) {
  if (!current || !next) return undefined;
  const a = current.currency === sub.currency ? current.unit_amount : current.currency_options[sub.currency]?.unit_amount;
  const b = next.currency === sub.currency ? next.unit_amount : next.currency_options[sub.currency]?.unit_amount;
  if (a === undefined || b === undefined) return undefined;
  const start = Date.parse(sub.current_period_start);
  const end = Date.parse(sub.current_period_end);
  const remaining = Math.max(0, end - Math.max(now, start));
  const qty = sub.items[0]?.quantity ?? 1;
  const credit = -prorate(a * qty, remaining, end - start);
  const charge = prorate(b * qty, remaining, end - start);
  return { credit, charge, net: credit + charge, currency: sub.currency };
}
