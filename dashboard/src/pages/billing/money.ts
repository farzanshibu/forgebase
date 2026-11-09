/** Money is integer minor units plus an ISO 4217 code. It is formatted here with Intl and never held as a float. */

/** Digits after the decimal point for a currency, taken from Intl (JPY 0, USD 2, KWD 3). */
export function exponent(currency: string): number {
  try {
    return new Intl.NumberFormat("en", { style: "currency", currency }).resolvedOptions().maximumFractionDigits ?? 2;
  } catch {
    return 2;
  }
}

/** `150000, "USD"` -> `$1,500.00`. State stays integer minor units; only this display step divides, and Intl rounds to the currency's digits. */
export function formatMoney(minor: number, currency: string): string {
  const exp = exponent(currency);
  const shown = minor / 10 ** exp;
  try {
    return new Intl.NumberFormat("en", { style: "currency", currency, minimumFractionDigits: exp, maximumFractionDigits: exp }).format(shown);
  } catch {
    return `${shown.toFixed(exp)} ${currency}`;
  }
}

/** Parse a decimal string typed by an operator (`12.5`) into minor units, or `null` when it is not a valid amount. */
export function parseMinor(text: string, currency: string): number | null {
  const exp = exponent(currency);
  const m = /^(-?)(\d+)(?:\.(\d*))?$/.exec(text.trim());
  if (!m) return null;
  const frac = m[3] ?? "";
  if (frac.length > exp) return null;
  const n = Number(BigInt(m[2]!) * 10n ** BigInt(exp) + BigInt(frac.padEnd(exp, "0") || "0"));
  if (!Number.isSafeInteger(n)) return null;
  return m[1] ? -n : n;
}

/** Minor units as a plain decimal string for an input box (`1250 USD` -> `12.50`). */
export function minorToInput(minor: number, currency: string): string {
  const exp = exponent(currency);
  const neg = minor < 0;
  const abs = BigInt(Math.abs(minor));
  const base = 10n ** BigInt(exp);
  return `${neg ? "-" : ""}${abs / base}${exp ? "." + (abs % base).toString().padStart(exp, "0") : ""}`;
}

/** Basis points as a percentage: `1250` -> `12.5%`. */
export function formatBp(bp: number | null | undefined): string {
  if (bp === null || bp === undefined) return "n/a";
  const neg = bp < 0;
  const a = Math.abs(bp);
  const whole = Math.floor(a / 100);
  const frac = a % 100;
  return `${neg ? "-" : ""}${whole}${frac ? "." + String(frac).padStart(2, "0").replace(/0$/, "") : ""}%`;
}

/** `750, 1000` -> `750 / 1,000`. */
export function formatCount(n: number): string {
  return new Intl.NumberFormat("en").format(n);
}

export type MeterState = "ok" | "warn70" | "warn90" | "full";
/** Quota state from used and limit with integer math (70, 90 and 100 percent thresholds). */
export function meterState(used: number, limit: number | null): MeterState {
  if (limit === null || limit <= 0) return "ok";
  if (used * 100 >= limit * 100) return "full";
  if (used * 100 >= limit * 90) return "warn90";
  if (used * 100 >= limit * 70) return "warn70";
  return "ok";
}

/** Whole percent of `used` against `limit`, capped at 100 for a bar width. */
export function percent(used: number, limit: number | null): number {
  if (limit === null || limit <= 0) return 0;
  return Math.min(100, Math.floor((used * 100) / limit));
}
