import { useState } from "react";
import { deleteStoreProduct, listPrices, listStoreProducts, listStorePurchases, putStoreProduct, STORE_PROVIDERS } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { Badge, BillingError, Empty, Field, SubjectLink, Th } from "./shared";
import { FORM_GRID, ROW, TABLE, THEAD, when_ } from "./util";
import { useAct } from "./useAct";

/** Store states that grant access are green; the rest follow the shared tones through this map. */
const PURCHASE_TONE: Record<string, string> = { grace: "past_due", on_hold: "paused", expired: "canceled", revoked: "refunded", replaced: "canceled" };
const PurchaseBadge = ({ status }: { status: string }) => <Badge status={PURCHASE_TONE[status] ?? status} label={status} />;

function StoreProducts() {
  const products = useLoad(listStoreProducts, "store-products");
  const prices = useLoad((s) => listPrices(undefined, s), "store-prices");
  const act = useAct(products.reload);
  const [provider, setProvider] = useState<string>(STORE_PROVIDERS[0]!);
  const [productId, setProductId] = useState("");
  const [price, setPrice] = useState("");
  const [filter, setFilter] = useState("");
  const rows = (products.data ?? []).filter((p) => !filter || p.provider === filter);
  const edit = (p: { provider: string; product_id: string; price: string }) => { setProvider(p.provider); setProductId(p.product_id); setPrice(p.price); };
  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    const ok = await act.run(() => putStoreProduct(provider, productId.trim(), price), "Mapping saved");
    if (ok) { setProductId(""); setPrice(""); }
  };
  return (
    <Panel title="Store products" actions={<label className="text-sm">Store<select aria-label="Filter store" className={`${inputCls} ml-2`} value={filter} onChange={(e) => setFilter(e.target.value)}><option value="">all</option>{STORE_PROVIDERS.map((p) => <option key={p}>{p}</option>)}</select></label>}>
      <BillingError error={products.error} />
      {products.loading && !products.error && <Loading what="Loading store products" />}
      {products.data && rows.length === 0 && <Empty>No store products mapped. A store product id maps to one price: recurring for a store subscription, one-time otherwise.</Empty>}
      {rows.length > 0 && (
        <table aria-label="Store products" className={TABLE}>
          <thead className={THEAD}><tr><Th>Store</Th><Th>Store product id</Th><Th>Price</Th><Th>Kind</Th><Th>Changed</Th><Th /></tr></thead>
          <tbody>
            {rows.map((p) => (
              <tr key={`${p.provider}/${p.product_id}`} className={ROW}>
                <td className="py-2 pr-3">{p.provider}</td><td className="font-mono text-xs">{p.product_id}</td><td className="font-mono text-xs">{p.price}</td>
                <td>{p.kind}</td><td className="text-zinc-400">{when_(p.updated_at)}</td>
                <td className="space-x-2 whitespace-nowrap">
                  <Button onClick={() => edit(p)} aria-label={`Edit ${p.provider} ${p.product_id}`}>Edit</Button>
                  <ConfirmButton label={`Delete ${p.provider} ${p.product_id}`} disabled={act.busy} onConfirm={() => void act.run(() => deleteStoreProduct(p.provider, p.product_id), "Mapping deleted")} />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <form onSubmit={save} aria-label="Store product mapping" className="mt-4 space-y-3 border-t border-zinc-800 pt-4">
        <div className={FORM_GRID}>
          <Field label="Mapping store"><select className={inputCls} value={provider} onChange={(e) => setProvider(e.target.value)}>{STORE_PROVIDERS.map((p) => <option key={p}>{p}</option>)}</select></Field>
          <Field label="Store product id"><input className={inputCls} value={productId} onChange={(e) => setProductId(e.target.value)} required placeholder="com.example.pro.monthly" /></Field>
          <Field label="Sold as price">
            <select className={inputCls} value={price} onChange={(e) => setPrice(e.target.value)} required>
              <option value="">choose a price</option>
              {(prices.data ?? []).filter((p) => p.active && p.type !== "metered").map((p) => <option key={p.key} value={p.key}>{p.key} ({p.type})</option>)}
            </select>
          </Field>
        </div>
        <ErrorView error={act.error} />
        {act.note && <p role="status" className="text-sm text-emerald-300">{act.note}</p>}
        <Button type="submit" tone="primary" disabled={act.busy}>Save mapping</Button>
        <p className="text-xs text-zinc-500">Saving an existing store product id re-maps it. The price type decides what is delivered: a recurring price becomes a subscription, a one-time price an order.</p>
      </form>
    </Panel>
  );
}

function StorePurchases() {
  const [subject, setSubject] = useState("");
  const [status, setStatus] = useState("");
  const purchases = useLoad((s) => listStorePurchases(subject || undefined, s), `store-purchases:${subject}`);
  const rows = (purchases.data ?? []).filter((p) => !status || p.status === status);
  return (
    <Panel title="Store purchases" actions={<Button onClick={purchases.reload}>Refresh</Button>}>
      <div className="mb-3 flex flex-wrap items-end gap-3 text-sm">
        <label>Status<select className={`${inputCls} ml-2`} value={status} onChange={(e) => setStatus(e.target.value)}><option value="">all</option>{["active", "trialing", "grace", "on_hold", "paused", "expired", "refunded", "revoked", "pending", "replaced"].map((s) => <option key={s}>{s}</option>)}</select></label>
        <label>Subject<input className={`${inputCls} ml-2 w-56`} value={subject} onChange={(e) => setSubject(e.target.value)} placeholder="all subjects" /></label>
      </div>
      <BillingError error={purchases.error} />
      {purchases.loading && !purchases.error && <Loading what="Loading store purchases" />}
      {purchases.data && rows.length === 0 && <Empty>No store purchases match.</Empty>}
      {rows.length > 0 && (
        <table aria-label="Store purchases" className={TABLE}>
          <thead className={THEAD}><tr><Th>Purchase</Th><Th>Store</Th><Th>Subject</Th><Th>Status</Th><Th>Product</Th><Th>Price</Th><Th>Renews</Th><Th>Expires</Th><Th>Mode</Th></tr></thead>
          <tbody>
            {rows.map((p) => (
              <tr key={p.id} className={ROW}>
                <td className="py-2 pr-3 font-mono text-xs">{p.purchase_id}</td><td>{p.provider}</td><td><SubjectLink subject={p.subject} /></td>
                <td><PurchaseBadge status={p.status} /></td><td className="font-mono text-xs">{p.product_id}</td><td className="font-mono text-xs">{p.price}</td>
                <td>{p.kind === "subscription" ? (p.auto_renew ? "auto-renews" : "ends") : "one-time"}</td><td className="text-zinc-400">{p.expires_at ? when_(p.expires_at) : "-"}</td>
                <td>{p.environment}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </Panel>
  );
}

export default function Stores() {
  return (
    <div className="space-y-4">
      <p className="text-sm text-zinc-400">App Store, Google Play and Creem are merchants of record: the store takes the payment, ForgeBase verifies and mirrors it. Credentials are set under Providers and settings.</p>
      <StoreProducts />
      <StorePurchases />
    </div>
  );
}
