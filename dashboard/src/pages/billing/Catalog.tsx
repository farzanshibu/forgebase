import { useState } from "react";
import { PRICE_TYPES, PRODUCT_KINDS, SCHEMES, listAddons, listFeatures, listPlans, listPrices, listProducts, listUsagePacks, putPrice, putProduct } from "../../api";
import type { BillingInterval, PriceInput, PriceScheme, PriceType, Product, ProductKind } from "../../api";
import { Button, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { describePrice, parseCurrencyOptions, parseTiers } from "./catalogUtil";
import { formatMoney, parseMinor } from "./money";
import { Badge, BillingError, Empty, Field, Th } from "./shared";
import { FORM_GRID, ROW, TABLE, THEAD } from "./util";
import { useAct } from "./useAct";

function ProductForm({ onSaved }: { onSaved: () => void }) {
  const act = useAct(onSaved);
  const [key, setKey] = useState("");
  const [name, setName] = useState("");
  const [kind, setKind] = useState<ProductKind>("plan");
  const [ref, setRef] = useState("");
  const [credits, setCredits] = useState("");
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    const input = {
      name, kind, active: true,
      plan_key: kind === "plan" ? ref || null : null, addon_key: kind === "addon" ? ref || null : null, pack_key: kind === "usage_pack" ? ref || null : null,
      credits: kind === "credits" && credits ? Number(credits) : null,
    };
    await act.run(() => putProduct(key, input), "Product saved");
  };
  return (
    <form onSubmit={submit} className="space-y-3" aria-label="New product">
      <div className={FORM_GRID}>
        <Field label="Product key"><input className={inputCls} value={key} onChange={(e) => setKey(e.target.value)} required /></Field>
        <Field label="Product name"><input className={inputCls} value={name} onChange={(e) => setName(e.target.value)} required /></Field>
        <Field label="Product kind">
          <select className={inputCls} value={kind} onChange={(e) => setKind(e.target.value as ProductKind)}>{PRODUCT_KINDS.map((k) => <option key={k}>{k}</option>)}</select>
        </Field>
        {(kind === "plan" || kind === "addon" || kind === "usage_pack") && (
          <Field label="Entitlement key"><input className={inputCls} value={ref} onChange={(e) => setRef(e.target.value)} placeholder={`${kind} key in Entitlements`} /></Field>
        )}
        {kind === "credits" && <Field label="Credits granted"><input className={inputCls} inputMode="numeric" value={credits} onChange={(e) => setCredits(e.target.value)} /></Field>}
      </div>
      <ErrorView error={act.error} />
      {act.note && <p role="status" className="text-sm text-emerald-300">{act.note}</p>}
      <Button type="submit" tone="primary" disabled={act.busy}>Save product</Button>
    </form>
  );
}

function PriceForm({ products, onSaved }: { products: Product[]; onSaved: () => void }) {
  const act = useAct(onSaved);
  const [key, setKey] = useState("");
  const [product, setProduct] = useState(products[0]?.key ?? "");
  const [type, setType] = useState<PriceType>("recurring");
  const [interval, setInterval_] = useState<BillingInterval>("month");
  const [scheme, setScheme] = useState<PriceScheme>("per_unit");
  const [currency, setCurrency] = useState("USD");
  const [amount, setAmount] = useState("");
  const [extra, setExtra] = useState("");
  const [tiers, setTiers] = useState("");
  const [trial, setTrial] = useState("0");
  const [feature, setFeature] = useState("");
  const [localError, setLocalError] = useState<string>();
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setLocalError(undefined);
    try {
      const cur = currency.trim().toUpperCase();
      const unit = parseMinor(amount || "0", cur);
      if (unit === null) throw new Error(`${amount} is not a valid ${cur} amount`);
      const input: PriceInput = {
        product, type, currency: cur, unit_amount: unit, interval: type === "recurring" ? interval : null, interval_count: 1,
        scheme, tiers: scheme === "per_unit" ? null : parseTiers(tiers, cur), currency_options: parseCurrencyOptions(extra),
        feature: type === "metered" ? feature || null : null, trial_days: Number(trial) || 0, active: true,
      };
      await act.run(() => putPrice(key, input), "Price saved");
    } catch (err) {
      setLocalError(err instanceof Error ? err.message : String(err));
    }
  };
  return (
    <form onSubmit={submit} className="space-y-3" aria-label="New price">
      <div className={FORM_GRID}>
        <Field label="Price key"><input className={inputCls} value={key} onChange={(e) => setKey(e.target.value)} required /></Field>
        <Field label="Price product">
          <select className={inputCls} value={product} onChange={(e) => setProduct(e.target.value)}>{products.map((p) => <option key={p.key} value={p.key}>{p.name}</option>)}</select>
        </Field>
        <Field label="Price type"><select className={inputCls} value={type} onChange={(e) => setType(e.target.value as PriceType)}>{PRICE_TYPES.map((t) => <option key={t}>{t}</option>)}</select></Field>
        {type === "recurring" && <Field label="Interval"><select className={inputCls} value={interval} onChange={(e) => setInterval_(e.target.value as BillingInterval)}><option>month</option><option>year</option></select></Field>}
        <Field label="Scheme"><select className={inputCls} value={scheme} onChange={(e) => setScheme(e.target.value as PriceScheme)}>{SCHEMES.map((t) => <option key={t}>{t}</option>)}</select></Field>
        <Field label="Currency"><input className={inputCls} value={currency} maxLength={3} onChange={(e) => setCurrency(e.target.value)} /></Field>
        <Field label="Unit amount"><input className={inputCls} inputMode="decimal" value={amount} onChange={(e) => setAmount(e.target.value)} placeholder="12.50" /></Field>
        <Field label="Trial days"><input className={inputCls} inputMode="numeric" value={trial} onChange={(e) => setTrial(e.target.value)} /></Field>
        {type === "metered" && <Field label="Metered feature"><input className={inputCls} value={feature} onChange={(e) => setFeature(e.target.value)} /></Field>}
      </div>
      <div className="grid gap-3 sm:grid-cols-2">
        <Field label="Other currencies (one per line, EUR=9.00)"><textarea className={`${inputCls} h-20 w-full font-mono`} value={extra} onChange={(e) => setExtra(e.target.value)} /></Field>
        {scheme !== "per_unit" && <Field label="Tiers (up_to, unit, flat per line)"><textarea className={`${inputCls} h-20 w-full font-mono`} value={tiers} onChange={(e) => setTiers(e.target.value)} placeholder={"100, 5.00, 0\ninf, 4.00, 0"} /></Field>}
      </div>
      {localError && <p role="alert" className="text-sm text-red-300">{localError}</p>}
      <ErrorView error={act.error} />
      {act.note && <p role="status" className="text-sm text-emerald-300">{act.note}</p>}
      <Button type="submit" tone="primary" disabled={act.busy || products.length === 0}>Save price</Button>
    </form>
  );
}

function EntitlementLists() {
  const plans = useLoad(listPlans, "plans");
  const addons = useLoad(listAddons, "addons");
  const packs = useLoad(listUsagePacks, "packs");
  const features = useLoad(listFeatures, "features");
  const bad = plans.error ?? addons.error ?? packs.error ?? features.error;
  return (
    <Panel title="Features, add-ons and usage packs (from Entitlements)">
      {bad ? <p className="text-sm text-zinc-400">The Entitlements catalog is not available on this server.</p> : null}
      <div className="grid gap-4 md:grid-cols-2">
        <div><h3 className="text-xs uppercase text-zinc-500">Plans</h3>
          <ul aria-label="Plans" className="mt-1 text-sm">{(plans.data ?? []).map((p) => <li key={p.key}>{p.name} <span className="text-zinc-500">({p.features.length} features{p.trial_days ? `, ${p.trial_days}-day trial` : ""})</span></li>)}</ul></div>
        <div><h3 className="text-xs uppercase text-zinc-500">Features</h3>
          <ul aria-label="Features" className="mt-1 text-sm">{(features.data ?? []).map((f) => <li key={f.key}>{f.name || f.key} <span className="text-zinc-500">({f.kind}{f.unit ? `, ${f.unit}` : ""})</span></li>)}</ul></div>
        <div><h3 className="text-xs uppercase text-zinc-500">Add-ons</h3>
          <ul aria-label="Add-ons" className="mt-1 text-sm">{(addons.data ?? []).map((a) => <li key={a.key}>{a.name}</li>)}</ul></div>
        <div><h3 className="text-xs uppercase text-zinc-500">Usage packs</h3>
          <ul aria-label="Usage packs" className="mt-1 text-sm">{(packs.data ?? []).map((p) => <li key={p.key}>{p.name} <span className="text-zinc-500">({new Intl.NumberFormat("en").format(p.units)} {p.feature})</span></li>)}</ul></div>
      </div>
    </Panel>
  );
}

export default function Catalog() {
  const products = useLoad(listProducts, "products");
  const prices = useLoad((s) => listPrices(undefined, s), "prices");
  const reload = () => {
    products.reload();
    prices.reload();
  };
  const byProduct = (key: string) => (prices.data ?? []).filter((p) => p.product === key);
  return (
    <div className="space-y-4">
      <BillingError error={products.error} />
      {products.loading && !products.error && <Loading what="Loading catalog" />}
      {products.data && (
        <Panel title="Products and prices">
          {products.data.length === 0 && <Empty>No products yet. Add one below.</Empty>}
          {products.data.length > 0 && (
            <table aria-label="Products" className={TABLE}>
              <thead className={THEAD}><tr><Th>Product</Th><Th>Kind</Th><Th>Prices</Th><Th>State</Th></tr></thead>
              <tbody>
                {products.data.map((p) => (
                  <tr key={p.key} className={`${ROW} align-top`}>
                    <td className="py-2 pr-3"><span className="font-medium">{p.name}</span><br /><span className="font-mono text-xs text-zinc-500">{p.key}</span></td>
                    <td className="pr-3">{p.kind}{p.credits ? ` (${new Intl.NumberFormat("en").format(p.credits)} credits)` : ""}</td>
                    <td className="pr-3">
                      <ul>
                        {byProduct(p.key).map((pr) => (
                          <li key={pr.key} className="text-sm">
                            <span className="font-mono text-xs text-zinc-500">{pr.key}</span> {pr.type} {describePrice(pr)}
                            {Object.entries(pr.currency_options).map(([c, o]) => <span key={c} className="ml-2 text-zinc-400">{formatMoney(o.unit_amount, c)}</span>)}
                            {pr.trial_days > 0 && <span className="ml-2 text-sky-300">{pr.trial_days}-day trial</span>}
                            {pr.feature && <span className="ml-2 text-zinc-400">metered {pr.feature} ({pr.meter_basis})</span>}
                            {!pr.active && <span className="ml-2"><Badge status="canceled" /></span>}
                          </li>
                        ))}
                        {byProduct(p.key).length === 0 && <li className="text-zinc-500">no prices</li>}
                      </ul>
                    </td>
                    <td><Badge status={p.active ? "active" : "canceled"} /></td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </Panel>
      )}
      <div className="grid gap-4 lg:grid-cols-2">
        <Panel title="New product"><ProductForm onSaved={reload} /></Panel>
        <Panel title="New price"><PriceForm key={products.data?.length ?? 0} products={products.data ?? []} onSaved={reload} /></Panel>
      </div>
      <EntitlementLists />
    </div>
  );
}
