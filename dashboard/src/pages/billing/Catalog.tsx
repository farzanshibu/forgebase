import { Textarea } from "panelui-web/textarea";
import { EmptyState } from "../../components/EmptyState";
import { useState } from "react";
import { PRICE_TYPES, PRODUCT_KINDS, SCHEMES, listAddons, listFeatures, listPlans, listPrices, listProducts, listUsagePacks, putPrice, putProduct, archivePrice, archiveProduct } from "../../api";
import type { BillingInterval, Price, PriceInput, PriceScheme, PriceType, Product, ProductKind } from "../../api";
import { Button, ConfirmButton, ErrorView } from "../../components/ui";
import { Sel } from "../../components/controls";
import { CreateSheet } from "../../components/CreateSheet";
import { FormField } from "../../components/FormField";
import { SkeletonTable } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import { describePrice, parseCurrencyOptions, parseTiers } from "./catalogUtil";
import { formatCount, formatMoney, parseMinor } from "./money";
import { BillingError, FormSection, Note, TableCard, Tag } from "./shared";
import { FORM_GRID, words } from "./util";
import { useAct } from "./useAct";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { Badge } from "../../components/StatusBadge";
import { Th } from "../../components/TableCard";

function ProductSheet({ open, onOpenChange, onSaved }: { open: boolean; onOpenChange: (v: boolean) => void; onSaved: () => void }) {
  const [key, setKey] = useState("");
  const [name, setName] = useState("");
  const [kind, setKind] = useState<ProductKind>("plan");
  const [ref, setRef] = useState("");
  const [credits, setCredits] = useState("");
  const creditsBad = kind === "credits" && credits !== "" && !/^\d+$/.test(credits.trim());
  return (
    <CreateSheet
      open={open}
      onOpenChange={onOpenChange}
      title="New product"
      description="A product is something you sell: a plan, an add-on, a usage pack, credits or a service. Give it one or more prices next."
      submitLabel="Save product"
      canSubmit={!!key.trim() && !!name.trim() && !creditsBad}
      onSubmit={async () => {
        await putProduct(key.trim(), {
          name, kind, active: true,
          plan_key: kind === "plan" ? ref || null : null, addon_key: kind === "addon" ? ref || null : null, pack_key: kind === "usage_pack" ? ref || null : null,
          credits: kind === "credits" && credits ? Number(credits) : null,
        });
        onSaved();
        const what = `Product ${name}`;
        setKey(""); setName(""); setRef(""); setCredits("");
        return { what };
      }}
    >
      <FormField label="Product key" help="A stable id your code uses, such as pro or team. Saving an existing key replaces that product.">
        {(p) => <Input {...p} size="sm" className="font-mono" value={key} onChange={(e) => setKey(e.target.value)} placeholder="pro" />}
      </FormField>
      <FormField label="Product name" help="What customers see on invoices.">
        {(p) => <Input {...p} size="sm" value={name} onChange={(e) => setName(e.target.value)} placeholder="Pro plan" />}
      </FormField>
      <FormField label="Product kind">
        {(p) => <Sel {...p} value={kind} onChange={(v) => setKind(v as ProductKind)} options={PRODUCT_KINDS.map((k) => ({ value: k, label: words(k) }))} />}
      </FormField>
      {(kind === "plan" || kind === "addon" || kind === "usage_pack") && (
        <FormField label="Entitlement key" help={`The ${words(kind)} in Entitlements that a purchase grants.`}>
          {(p) => <Input {...p} size="sm" className="font-mono" value={ref} onChange={(e) => setRef(e.target.value)} placeholder={`${words(kind)} key`} />}
        </FormField>
      )}
      {kind === "credits" && (
        <FormField label="Credits granted" error={creditsBad ? "A whole number, like 1000" : undefined}>
          {(p) => <Input {...p} size="sm" inputMode="numeric" value={credits} onChange={(e) => setCredits(e.target.value)} placeholder="1000" />}
        </FormField>
      )}
    </CreateSheet>
  );
}

function PriceSheet({ open, onOpenChange, products, onSaved }: { open: boolean; onOpenChange: (v: boolean) => void; products: Product[]; onSaved: () => void }) {
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
  const cur = currency.trim().toUpperCase();
  const curBad = !/^[A-Z]{3}$/.test(cur);
  const amountBad = !curBad && amount !== "" && parseMinor(amount, cur) === null;
  const trialBad = trial !== "" && !/^\d+$/.test(trial.trim());
  return (
    <CreateSheet
      open={open}
      onOpenChange={onOpenChange}
      title="New price"
      description="How much a product costs, in which currency, and how often."
      submitLabel="Save price"
      canSubmit={!!key.trim() && !!product && !curBad && !amountBad && !trialBad}
      onSubmit={async () => {
        const unit = parseMinor(amount || "0", cur);
        if (unit === null) throw new Error(`${amount} is not a valid ${cur} amount`);
        const input: PriceInput = {
          product, type, currency: cur, unit_amount: unit, interval: type === "recurring" ? interval : null, interval_count: 1,
          scheme, tiers: scheme === "per_unit" ? null : parseTiers(tiers, cur), currency_options: parseCurrencyOptions(extra),
          feature: type === "metered" ? feature || null : null, trial_days: Number(trial) || 0, active: true,
        };
        await putPrice(key.trim(), input);
        onSaved();
        const what = `Price ${key.trim()}`;
        setKey(""); setAmount(""); setExtra(""); setTiers(""); setFeature("");
        return { what };
      }}
    >
      <FormSection title="Basics">
        <div className={FORM_GRID}>
          <FormField label="Price key" help="A stable id, such as pro_month.">
            {(p) => <Input {...p} size="sm" className="font-mono" value={key} onChange={(e) => setKey(e.target.value)} placeholder="pro_month" />}
          </FormField>
          <FormField label="Price product">
            {(p) => <Sel {...p} value={product} onChange={setProduct} placeholder="Choose a product" options={products.map((x) => ({ value: x.key, label: x.name }))} />}
          </FormField>
          <FormField label="Price type">
            {(p) => <Sel {...p} value={type} onChange={(v) => setType(v as PriceType)} options={PRICE_TYPES.map((t) => ({ value: t, label: words(t) }))} />}
          </FormField>
          {type === "recurring" && (
            <FormField label="Interval">
              {(p) => <Sel {...p} value={interval} onChange={(v) => setInterval_(v as BillingInterval)} options={[{ value: "month", label: "Monthly" }, { value: "year", label: "Yearly" }]} />}
            </FormField>
          )}
          {type === "metered" && (
            <FormField label="Metered feature" help="The Entitlements feature whose usage is billed.">
              {(p) => <Input {...p} size="sm" className="font-mono" value={feature} onChange={(e) => setFeature(e.target.value)} placeholder="api_calls" />}
            </FormField>
          )}
        </div>
      </FormSection>
      <FormSection title="Amount" description="Amounts are typed in the currency's own units (12.50 for USD) and stored exactly.">
        <div className={FORM_GRID}>
          <FormField label="Currency" error={curBad ? "A three-letter code, like USD" : undefined}>
            {(p) => <Input {...p} size="sm" className="font-mono uppercase" value={currency} maxLength={3} onChange={(e) => setCurrency(e.target.value)} />}
          </FormField>
          <FormField label="Unit amount" error={amountBad ? `${amount} is not a valid ${cur} amount` : undefined}>
            {(p) => <Input {...p} size="sm" className="tabular-nums" inputMode="decimal" value={amount} onChange={(e) => setAmount(e.target.value)} placeholder="12.50" />}
          </FormField>
          <FormField label="Scheme" help="Per unit, or tiers that change the price with quantity.">
            {(p) => <Sel {...p} value={scheme} onChange={(v) => setScheme(v as PriceScheme)} options={SCHEMES.map((s) => ({ value: s, label: words(s) }))} />}
          </FormField>
        </div>
        {scheme !== "per_unit" && (
          <FormField label="Tiers" help="One tier per line: up to, unit amount, flat amount. Use inf for the last tier.">
            {(p) => <Textarea {...p} minRows={3} className="w-full font-mono" value={tiers} onChange={(e) => setTiers(e.target.value)} placeholder={"100, 5.00, 0\ninf, 4.00, 0"} />}
          </FormField>
        )}
        <FormField label="Other currencies" help="Optional. One per line, like EUR=9.00.">
          {(p) => <Textarea {...p} minRows={2} className="w-full font-mono" value={extra} onChange={(e) => setExtra(e.target.value)} placeholder="EUR=9.00" />}
        </FormField>
      </FormSection>
      <FormSection title="Trial">
        <FormField label="Trial days" help="0 for no trial." error={trialBad ? "A whole number of days" : undefined}>
          {(p) => <Input {...p} size="sm" className="w-32 tabular-nums" inputMode="numeric" value={trial} onChange={(e) => setTrial(e.target.value)} />}
        </FormField>
      </FormSection>
    </CreateSheet>
  );
}

function PriceLine({ pr, busy, onArchive }: { pr: Price; busy: boolean; onArchive: () => void }) {
  return (
    <li className="flex flex-wrap items-center gap-x-2 gap-y-1">
      <span className={`font-medium tabular-nums ${pr.active ? "" : "text-muted-foreground line-through"}`.trim()}>{describePrice(pr)}</span>
      {Object.entries(pr.currency_options).map(([c, o]) => <span key={c} className="text-muted-foreground tabular-nums">{formatMoney(o.unit_amount, c)}</span>)}
      <Tag>{words(pr.type)}</Tag>
      <span className="font-mono text-xs text-muted-foreground">{pr.key}</span>
      {pr.trial_days > 0 && <span className="text-xs text-info">{pr.trial_days}-day trial</span>}
      {pr.feature && <span className="text-xs text-muted-foreground">metered {pr.feature} ({pr.meter_basis})</span>}
      {!pr.active && <Badge status="archived" />}
      {pr.active && <span className="ml-auto"><ConfirmButton className="h-6 border-transparent px-2 text-xs" label={`Archive price ${pr.key}`} short="Archive" consequence="New subscriptions and orders can no longer use it. Existing ones keep it." disabled={busy} onConfirm={onArchive} /></span>}
    </li>
  );
}

function EntitlementLists() {
  const plans = useLoad(listPlans, "plans");
  const addons = useLoad(listAddons, "addons");
  const packs = useLoad(listUsagePacks, "packs");
  const features = useLoad(listFeatures, "features");
  const bad = plans.error ?? addons.error ?? packs.error ?? features.error;
  const col = (title: string, items: { key: string; text: string; meta?: string }[]) => (
    <div className="min-w-0 rounded-lg border border-border p-3">
      <h3 className="flex items-center justify-between text-xs font-medium text-muted-foreground">{title}<span className="tabular-nums">{items.length}</span></h3>
      {items.length === 0 && <p className="mt-2 text-sm text-muted-foreground">None defined.</p>}
      <ul aria-label={title} className="mt-2 space-y-1 text-sm">
        {items.map((i) => <li key={i.key} className="truncate">{i.text} {i.meta && <span className="text-muted-foreground">({i.meta})</span>}</li>)}
      </ul>
    </div>
  );
  return (
    <TableCard title="From Entitlements" description="Plans, features, add-ons and usage packs a product can grant. Edit them in Entitlements.">
      <div className="p-4">
        {bad ? <p className="mb-3 text-sm text-muted-foreground">The Entitlements catalog is not available on this server.</p> : null}
        <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
          {col("Plans", (plans.data ?? []).map((p) => ({ key: p.key, text: p.name, meta: `${p.features.length} features${p.trial_days ? `, ${p.trial_days}-day trial` : ""}` })))}
          {col("Features", (features.data ?? []).map((f) => ({ key: f.key, text: f.name || f.key, meta: `${f.kind}${f.unit ? `, ${f.unit}` : ""}` })))}
          {col("Add-ons", (addons.data ?? []).map((a) => ({ key: a.key, text: a.name })))}
          {col("Usage packs", (packs.data ?? []).map((p) => ({ key: p.key, text: p.name, meta: `${formatCount(p.units)} ${p.feature}` })))}
        </div>
      </div>
    </TableCard>
  );
}

export default function Catalog() {
  const products = useLoad(listProducts, "products");
  const prices = useLoad((s) => listPrices(undefined, s), "prices");
  const [newProduct, setNewProduct] = useState(false);
  const [newPrice, setNewPrice] = useState(false);
  const reload = () => {
    products.reload();
    prices.reload();
  };
  const act = useAct(reload);
  const byProduct = (key: string) => (prices.data ?? []).filter((p) => p.product === key);
  const list = products.data ?? [];
  const actions = (
    <>
      <Button onClick={() => setNewPrice(true)} disabled={list.length === 0} title={list.length === 0 ? "Add a product first" : undefined}>New price</Button>
      <Button tone="primary" onClick={() => setNewProduct(true)}>New product</Button>
    </>
  );
  return (
    <div className="space-y-5">
      <BillingError error={products.error} />
      <TableCard title="Products and prices" count={products.data ? list.length : undefined} description="What you sell and what it costs. Archived items stay on past invoices." actions={actions}>
        {products.loading && !products.error && <SkeletonTable rows={3} columns={4} label="Loading catalog" />}
        {products.data && list.length === 0 && (
          <EmptyState
            title="No products yet"
            why="A product is something you sell, such as a plan or a credit pack. Add one, then give it a price."
            action={<Button tone="primary" onClick={() => setNewProduct(true)}>New product</Button>}
            docs="docs/adr/0021-billing.md"
          />
        )}
        {list.length > 0 && (
          <Table framed aria-label="Products">
            <Table.Header><Table.Row><Th>Product</Th><Th>Kind</Th><Th>Prices</Th><Th>Status</Th><Th><span className="sr-only">Actions</span></Th></Table.Row></Table.Header>
            <Table.Body>
              {list.map((p) => (
                <Table.Row key={p.key} className="align-top">
                  <Table.Cell className="min-w-40">
                    <span className="block font-medium">{p.name}</span>
                    <span className="font-mono text-xs text-muted-foreground">{p.key}</span>
                  </Table.Cell>
                  <Table.Cell>
                    <Tag>{words(p.kind)}</Tag>
                    {p.credits ? <span className="mt-1 block whitespace-nowrap text-xs text-muted-foreground tabular-nums">{formatCount(p.credits)} credits</span> : null}
                  </Table.Cell>
                  <Table.Cell>
                    <ul className="space-y-1.5">
                      {byProduct(p.key).map((pr) => (
                        <PriceLine key={pr.key} pr={pr} busy={act.busy} onArchive={() => void act.run(() => archivePrice(pr.key), "Price archived")} />
                      ))}
                      {byProduct(p.key).length === 0 && <li className="text-muted-foreground">No prices yet</li>}
                    </ul>
                  </Table.Cell>
                  <Table.Cell><Badge status={p.active ? "active" : "archived"} /></Table.Cell>
                  <Table.Cell className="text-right">
                    {p.active && <ConfirmButton label={`Archive product ${p.key}`} short="Archive" consequence="It can no longer be sold. Customers who have it keep it." disabled={act.busy} onConfirm={() => void act.run(() => archiveProduct(p.key), "Product archived")} />}
                  </Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
      <ErrorView error={act.error} />
      <Note>{act.note}</Note>
      <EntitlementLists />
      <ProductSheet open={newProduct} onOpenChange={setNewProduct} onSaved={reload} />
      <PriceSheet key={list.length} open={newPrice} onOpenChange={setNewPrice} products={list} onSaved={reload} />
    </div>
  );
}
