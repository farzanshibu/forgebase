import { describe, expect, it } from "vitest";
import { formatBp, formatMoney, meterState, minorToInput, parseMinor, percent, prorate } from "./money";
import { parseCurrencyOptions, parseTiers } from "./catalogUtil";
import { prorationPreview } from "./proration";
import { price } from "../../test/billingMock";
import type { Subscription } from "../../api";

describe("money", () => {
  it("formats integer minor units with the currency's own digits", () => {
    expect(formatMoney(150000, "USD")).toBe("$1,500.00");
    expect(formatMoney(5, "USD")).toBe("$0.05");
    expect(formatMoney(1500, "JPY")).toBe("¥1,500");
    expect(formatMoney(-2500, "EUR")).toBe("-€25.00");
    expect(formatMoney(1234, "KWD").replace(/\u00a0/g, " ")).toBe("KWD 1.234");
  });
  it("parses typed decimals to minor units exactly and rejects extra digits", () => {
    expect(parseMinor("12.5", "USD")).toBe(1250);
    expect(parseMinor("0.07", "USD")).toBe(7);
    expect(parseMinor("1500", "JPY")).toBe(1500);
    expect(parseMinor("1.5", "JPY")).toBeNull();
    expect(parseMinor("1.005", "USD")).toBeNull();
    expect(parseMinor("abc", "USD")).toBeNull();
    expect(minorToInput(1250, "USD")).toBe("12.50");
    expect(minorToInput(-7, "USD")).toBe("-0.07");
  });
  it("formats basis points", () => {
    expect(formatBp(1250)).toBe("12.5%");
    expect(formatBp(1800)).toBe("18%");
    expect(formatBp(null)).toBe("n/a");
  });
  it("classifies quota use at 70, 90 and 100 percent", () => {
    expect(meterState(699, 1000)).toBe("ok");
    expect(meterState(700, 1000)).toBe("warn70");
    expect(meterState(900, 1000)).toBe("warn90");
    expect(meterState(1000, 1000)).toBe("full");
    expect(meterState(5000, null)).toBe("ok");
    expect(percent(750, 1000)).toBe(75);
    expect(percent(2000, 1000)).toBe(100);
  });
  it("prorates with integers, rounding half away from zero", () => {
    expect(prorate(3000, 15, 30)).toBe(1500);
    expect(prorate(1, 1, 2)).toBe(1);
    expect(prorate(-1, 1, 2)).toBe(-1);
    expect(prorate(100, 0, 30)).toBe(0);
  });
  it("parses currency options and tiers", () => {
    expect(parseCurrencyOptions("EUR=9.00\nJPY=1500")).toEqual({ EUR: { unit_amount: 900 }, JPY: { unit_amount: 1500 } });
    expect(() => parseCurrencyOptions("EUR 9")).toThrow(/EUR=9.00/);
    expect(parseTiers("100, 5.00, 0\ninf, 4.00, 1.50", "USD")).toEqual([
      { up_to: 100, unit_amount: 500, flat_amount: 0 }, { up_to: null, unit_amount: 400, flat_amount: 150 },
    ]);
    expect(() => parseTiers("x, y", "USD")).toThrow();
  });
  it("previews a plan change from the unused part of the period", () => {
    const sub = { currency: "USD", current_period_start: "2026-11-01T00:00:00Z", current_period_end: "2026-12-01T00:00:00Z", items: [{ quantity: 1 }] } as unknown as Subscription;
    const mid = Date.parse("2026-11-16T00:00:00Z");
    const p = prorationPreview(sub, price("a", "x", { unit_amount: 2900 }), price("b", "x", { unit_amount: 9900 }), mid);
    expect(p).toEqual({ credit: -1450, charge: 4950, net: 3500, currency: "USD" });
    expect(prorationPreview(sub, undefined, price("b", "x"))).toBeUndefined();
  });
});
