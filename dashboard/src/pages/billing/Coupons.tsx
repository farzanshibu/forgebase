import { useState } from "react";
import { archiveCoupon, listCoupons, putCoupon } from "../../api";
import type { CouponDuration, CouponKind } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { csv } from "../functions/util";
import { formatBp, formatMoney, parseMinor } from "./money";
import { Badge, BillingError, Empty, Field, Th } from "./shared";
import { FORM_GRID, ROW, TABLE, THEAD, when_ } from "./util";
import { useAct } from "./useAct";

export default function Coupons() {
  const coupons = useLoad(listCoupons, "coupons");
  const act = useAct(coupons.reload);
  const [code, setCode] = useState("");
  const [kind, setKind] = useState<CouponKind>("percent");
  const [value, setValue] = useState("");
  const [currency, setCurrency] = useState("USD");
  const [duration, setDuration] = useState<CouponDuration>("once");
  const [cycles, setCycles] = useState("");
  const [max, setMax] = useState("");
  const [products, setProducts] = useState("");
  const [bad, setBad] = useState<string>();
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setBad(undefined);
    const cur = currency.toUpperCase();
    let percent_bp: number | null = null;
    let amount_off: number | null = null;
    if (kind === "percent") {
      const m = /^(\d+)(?:\.(\d{1,2}))?$/.exec(value.trim());
      if (!m) return setBad("Percent must look like 15 or 12.5");
      percent_bp = Number(m[1]) * 100 + Number((m[2] ?? "").padEnd(2, "0") || 0);
    } else {
      amount_off = parseMinor(value, cur);
      if (amount_off === null) return setBad(`${value} is not a valid ${cur} amount`);
    }
    await act.run(() => putCoupon(code, {
      kind, percent_bp, amount_off, currency: kind === "amount" ? cur : null, duration, duration_cycles: duration === "repeating" ? Number(cycles) || null : null,
      max_redemptions: max ? Number(max) : null, products: csv(products), active: true,
    }), "Coupon saved");
  };
  return (
    <div className="space-y-4">
      <BillingError error={coupons.error} />
      {coupons.loading && !coupons.error && <Loading what="Loading coupons" />}
      {coupons.data && coupons.data.length === 0 && <Empty>No coupons yet.</Empty>}
      {coupons.data && coupons.data.length > 0 && (
        <table aria-label="Coupons" className={TABLE}>
          <thead className={THEAD}><tr><Th>Code</Th><Th>Discount</Th><Th>Duration</Th><Th>Redeemed</Th><Th>Expires</Th><Th>State</Th><Th /></tr></thead>
          <tbody>
            {coupons.data.map((c) => (
              <tr key={c.code} className={ROW}>
                <td className="py-2 pr-3 font-mono text-xs">{c.code}</td>
                <td>{c.kind === "percent" ? `${formatBp(c.percent_bp)} off` : `${formatMoney(c.amount_off ?? 0, c.currency ?? "USD")} off`}</td>
                <td>{c.duration}{c.duration_cycles ? ` (${c.duration_cycles} cycles)` : ""}</td>
                <td>{c.redeemed}{c.max_redemptions ? ` / ${c.max_redemptions}` : ""}</td><td className="text-zinc-400">{c.expires_at ? when_(c.expires_at) : "never"}</td>
                <td><Badge status={c.active ? "active" : "canceled"} /></td>
                <td>{c.active && <ConfirmButton label={`Archive coupon ${c.code}`} disabled={act.busy} onConfirm={() => void act.run(() => archiveCoupon(c.code), "Coupon archived")} />}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <Panel title="New coupon">
        <form onSubmit={submit} aria-label="New coupon" className="space-y-3">
          <div className={FORM_GRID}>
            <Field label="Coupon code"><input className={inputCls} value={code} onChange={(e) => setCode(e.target.value)} required /></Field>
            <Field label="Coupon kind"><select className={inputCls} value={kind} onChange={(e) => setKind(e.target.value as CouponKind)}><option>percent</option><option>amount</option></select></Field>
            <Field label={kind === "percent" ? "Percent off" : "Amount off"}><input className={inputCls} inputMode="decimal" value={value} onChange={(e) => setValue(e.target.value)} required /></Field>
            {kind === "amount" && <Field label="Coupon currency"><input className={inputCls} maxLength={3} value={currency} onChange={(e) => setCurrency(e.target.value)} /></Field>}
            <Field label="Duration"><select className={inputCls} value={duration} onChange={(e) => setDuration(e.target.value as CouponDuration)}><option>once</option><option>repeating</option><option>forever</option></select></Field>
            {duration === "repeating" && <Field label="Cycles"><input className={inputCls} inputMode="numeric" value={cycles} onChange={(e) => setCycles(e.target.value)} /></Field>}
            <Field label="Max redemptions"><input className={inputCls} inputMode="numeric" value={max} onChange={(e) => setMax(e.target.value)} /></Field>
            <Field label="Only for products (comma separated)"><input className={inputCls} value={products} onChange={(e) => setProducts(e.target.value)} /></Field>
          </div>
          {bad && <p role="alert" className="text-sm text-red-300">{bad}</p>}
          <ErrorView error={act.error} />
          {act.note && <p role="status" className="text-sm text-emerald-300">{act.note}</p>}
          <Button type="submit" tone="primary" disabled={act.busy}>Save coupon</Button>
        </form>
      </Panel>
    </div>
  );
}
