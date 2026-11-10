import { Textarea } from "panelui-web/textarea";
import { useState } from "react";
import { PRICE_TYPES, PRODUCT_KINDS, SCHEMES, listAddons, listFeatures, listPlans, listPrices, listProducts, listUsagePacks, putPrice, putProduct, archivePrice, archiveProduct } from "../../api";
import type { BillingInterval, PriceInput, PriceScheme, PriceType, Product, ProductKind } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel } from "../../components/ui";
import { Sel } from "../../components/controls";
import { useLoad } from "../../hooks";
import { describePrice, parseCurrencyOptions, parseTiers } from "./catalogUtil";
import { formatMoney, parseMinor } from "./money";
import { Badge, BillingError, Empty, Field, Th } from "./shared";
import { FORM_GRID, ROW, TABLE, THEAD } from "./util";
import { useAct } from "./useAct";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";

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
        <Field label="Product key"><Input size="sm" value={key} onChange={(e) => setKey(e.target.value)} required /></Field>
        <Field label="Product name"><Input size="sm" value={name} onChange={(e) => setName(e.target.value)} required /></Field>
        <Field label="Product kind">
          <Sel value={kind} onChange={(v) => setKind(v as ProductKind)} options={PRODUCT_KINDS} />
        </Field>
        {(kind === "plan" || kind === "addon" || kind === "usage_pack") && (
          <Field label="Entitlement key"><Input size="sm" value={ref} onChange={(e) => setRef(e.target.value)} placeholder={`${kind} key in Entitlements`} /></Field>
        )}
        {kind === "credits" && <Field label="Credits granted"><Input size="sm" inputMode="numeric" value={credits} onChange={(e) => setCredits(e.target.value)} /></Field>}
      </div>
      <ErrorView error={act.error} />
      {act.note && <p role="status" className="text-sm text-success">{act.note}</p>}
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
        <Field label="Price key"><Input size="sm" value={key} onChange={(e) => setKey(e.target.value)} required /></Field>
        <Field label="Price product">
          <Sel value={product} onChange={setProduct} options={products.map((p) => ({ value: p.key, label: p.name }))} />
        </Field>
        <Field label="Price type"><Sel value={type} onChange={(v) => setType(v as PriceType)} options={PRICE_TYPES} /></Field>
        {type === "recurring" && <Field label="Interval"><Sel value={interval} onChange={(v) => setInterval_(v as BillingInterval)} options={["month", "year"]} /></Field>}
        <Field label="Scheme"><Sel value={scheme} onChange={(v) => setScheme(v as PriceScheme)} options={SCHEMES} /></Field>
        <Field label="Currency"><Input size="sm" value={currency} maxLength={3} onChange={(e) => setCurrency(e.target.value)} /></Field>
        <Field label="Unit amount"><Input size="sm" inputMode="decimal" value={amount} onChange={(e) => setAmount(e.target.value)} placeholder="12.50" /></Field>
        <Field label="Trial days"><Input size="sm" inputMode="numeric" value={trial} onChange={(e) => setTrial(e.target.value)} /></Field>
        {type === "metered" && <Field label="Metered feature"><Input size="sm" value={feature} onChange={(e) => setFeature(e.target.value)} /></Field>}
      </div>
      <div className="grid gap-3 sm:grid-cols-2">
        <Field label="Other currencies (one per line, EUR=9.00)"><Textarea minRows={4} className="w-full font-mono" value={extra} onChange={(e) => setExtra(e.target.value)} /></Field>
        {scheme !== "per_unit" && <Field label="Tiers (up_to, unit, flat per line)"><Textarea minRows={4} className="w-full font-mono" value={tiers} onChange={(e) => setTiers(e.target.value)} placeholder={"100, 5.00, 0\ninf, 4.00, 0"} /></Field>}
      </div>
      {localError && <p role="alert" className="text-sm text-destructive">{localError}</p>}
      <ErrorView error={act.error} />
      {act.note && <p role="status" className="text-sm text-success">{act.note}</p>}
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
      {bad ? <p className="text-sm text-muted-foreground">The Entitlements catalog is not available on this server.</p> : null}
      <div className="grid gap-4 md:grid-cols-2">
        <div><h3 className="text-xs uppercase text-muted-foreground">Plans</h3>
          <ul aria-label="Plans" className="mt-1 text-sm">{(plans.data ?? []).map((p) => <li key={p.key}>{p.name} <span className="text-muted-foreground">({p.features.length} features{p.trial_days ? `, ${p.trial_days}-day trial` : ""})</span></li>)}</ul></div>
        <div><h3 className="text-xs uppercase text-muted-foreground">Features</h3>
          <ul aria-label="Features" className="mt-1 text-sm">{(features.data ?? []).map((f) => <li key={f.key}>{f.name || f.key} <span className="text-muted-foreground">({f.kind}{f.unit ? `, ${f.unit}` : ""})</span></li>)}</ul></div>
        <div><h3 className="text-xs uppercase text-muted-foreground">Add-ons</h3>
          <ul aria-label="Add-ons" className="mt-1 text-sm">{(addons.data ?? []).map((a) => <li key={a.key}>{a.name}</li>)}</ul></div>
        <div><h3 className="text-xs uppercase text-muted-foreground">Usage packs</h3>
          <ul aria-label="Usage packs" className="mt-1 text-sm">{(packs.data ?? []).map((p) => <li key={p.key}>{p.name} <span className="text-muted-foreground">({new Intl.NumberFormat("en").format(p.units)} {p.feature})</span></li>)}</ul></div>
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
  const act = useAct(reload);
  const byProduct = (key: string) => (prices.data ?? []).filter((p) => p.product === key);
  return (
    <div className="space-y-4">
      <BillingError error={products.error} />
      {products.loading && !products.error && <Loading what="Loading catalog" />}
      {products.data && (
        <Panel title="Products and prices">
          {products.data.length === 0 && <Empty>No products yet. Add one below.</Empty>}
          {products.data.length > 0 && (
            <Table aria-label="Products" className={TABLE}>
              <Table.Header className={THEAD}><Table.Row><Th>Product</Th><Th>Kind</Th><Th>Prices</Th><Th>State</Th></Table.Row></Table.Header>
              <Table.Body>
                {products.data.map((p) => (
                  <Table.Row key={p.key} className={`${ROW} align-top`}>
                    <Table.Cell className="py-2 pr-3"><span className="font-medium">{p.name}</span><br /><span className="font-mono text-xs text-muted-foreground">{p.key}</span></Table.Cell>
                    <Table.Cell className="pr-3">{p.kind}{p.credits ? ` (${new Intl.NumberFormat("en").format(p.credits)} credits)` : ""}</Table.Cell>
                    <Table.Cell className="pr-3">
                      <ul>
                        {byProduct(p.key).map((pr) => (
                          <li key={pr.key} className="text-sm">
                            <span className="font-mono text-xs text-muted-foreground">{pr.key}</span> {pr.type} {describePrice(pr)}
                            {Object.entries(pr.currency_options).map(([c, o]) => <span key={c} className="ml-2 text-muted-foreground">{formatMoney(o.unit_amount, c)}</span>)}
                            {pr.trial_days > 0 && <span className="ml-2 text-info">{pr.trial_days}-day trial</span>}
                            {pr.feature && <span className="ml-2 text-muted-foreground">metered {pr.feature} ({pr.meter_basis})</span>}
                            {!pr.active && <span className="ml-2"><Badge status="canceled" /></span>}
                            {pr.active && <span className="ml-2"><ConfirmButton label={`Archive price ${pr.key}`} disabled={act.busy} onConfirm={() => void act.run(() => archivePrice(pr.key), "Price archived")} /></span>}
                          </li>
                        ))}
                        {byProduct(p.key).length === 0 && <li className="text-muted-foreground">no prices</li>}
                      </ul>
                    </Table.Cell>
                    <Table.Cell>
                      <Badge status={p.active ? "active" : "canceled"} />
                      {p.active && <div className="mt-1"><ConfirmButton label={`Archive product ${p.key}`} disabled={act.busy} onConfirm={() => void act.run(() => archiveProduct(p.key), "Product archived")} /></div>}
                    </Table.Cell>
                  </Table.Row>
                ))}
              </Table.Body>
            </Table>
          )}
          <ErrorView error={act.error} />
          {act.note && <p role="status" className="mt-2 text-sm text-success">{act.note}</p>}
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
