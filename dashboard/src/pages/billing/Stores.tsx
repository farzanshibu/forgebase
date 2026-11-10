import { useState } from "react";
import { deleteStoreProduct, listPrices, listStoreProducts, listStorePurchases, putStoreProduct, STORE_PROVIDERS } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel } from "../../components/ui";
import { Sel } from "../../components/controls";
import { useLoad } from "../../hooks";
import { Badge, BillingError, Empty, Field, SubjectLink, Th } from "./shared";
import { FORM_GRID, ROW, TABLE, THEAD, when_ } from "./util";
import { useAct } from "./useAct";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";

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
    <Panel title="Store products" actions={<label className="text-sm">Store<Sel aria-label="Filter store" value={filter} onChange={setFilter} options={[{ value: "", label: "all" }, ...STORE_PROVIDERS]} className="ml-2" /></label>}>
      <BillingError error={products.error} />
      {products.loading && !products.error && <Loading what="Loading store products" />}
      {products.data && rows.length === 0 && <Empty>No store products mapped. A store product id maps to one price: recurring for a store subscription, one-time otherwise.</Empty>}
      {rows.length > 0 && (
        <Table aria-label="Store products" className={TABLE}>
          <Table.Header className={THEAD}><Table.Row><Th>Store</Th><Th>Store product id</Th><Th>Price</Th><Th>Kind</Th><Th>Changed</Th><Th /></Table.Row></Table.Header>
          <Table.Body>
            {rows.map((p) => (
              <Table.Row key={`${p.provider}/${p.product_id}`} className={ROW}>
                <Table.Cell className="py-2 pr-3">{p.provider}</Table.Cell><Table.Cell className="font-mono text-xs">{p.product_id}</Table.Cell><Table.Cell className="font-mono text-xs">{p.price}</Table.Cell>
                <Table.Cell>{p.kind}</Table.Cell><Table.Cell className="text-muted-foreground">{when_(p.updated_at)}</Table.Cell>
                <Table.Cell className="space-x-2 whitespace-nowrap">
                  <Button onClick={() => edit(p)} aria-label={`Edit ${p.provider} ${p.product_id}`}>Edit</Button>
                  <ConfirmButton label={`Delete ${p.provider} ${p.product_id}`} disabled={act.busy} onConfirm={() => void act.run(() => deleteStoreProduct(p.provider, p.product_id), "Mapping deleted")} />
                </Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
      <form onSubmit={save} aria-label="Store product mapping" className="mt-4 space-y-3 border-t border-border pt-4">
        <div className={FORM_GRID}>
          <Field label="Mapping store"><Sel value={provider} onChange={setProvider} options={STORE_PROVIDERS} /></Field>
          <Field label="Store product id"><Input size="sm" value={productId} onChange={(e) => setProductId(e.target.value)} required placeholder="com.example.pro.monthly" /></Field>
          <Field label="Sold as price">
            <Sel value={price} onChange={setPrice} required options={[{ value: "", label: "choose a price" }, ...(prices.data ?? []).filter((p) => p.active && p.type !== "metered").map((p) => ({ value: p.key, label: `${p.key} (${p.type})` }))]} />
          </Field>
        </div>
        <ErrorView error={act.error} />
        {act.note && <p role="status" className="text-sm text-success">{act.note}</p>}
        <Button type="submit" tone="primary" disabled={act.busy}>Save mapping</Button>
        <p className="text-xs text-muted-foreground">Saving an existing store product id re-maps it. The price type decides what is delivered: a recurring price becomes a subscription, a one-time price an order.</p>
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
        <label>Status<Sel value={status} onChange={setStatus} options={[{ value: "", label: "all" }, ...["active", "trialing", "grace", "on_hold", "paused", "expired", "refunded", "revoked", "pending", "replaced"]]} className="ml-2" /></label>
        <label>Subject<Input size="sm" className="ml-2 w-56" value={subject} onChange={(e) => setSubject(e.target.value)} placeholder="all subjects" /></label>
      </div>
      <BillingError error={purchases.error} />
      {purchases.loading && !purchases.error && <Loading what="Loading store purchases" />}
      {purchases.data && rows.length === 0 && <Empty>No store purchases match.</Empty>}
      {rows.length > 0 && (
        <Table aria-label="Store purchases" className={TABLE}>
          <Table.Header className={THEAD}><Table.Row><Th>Purchase</Th><Th>Store</Th><Th>Subject</Th><Th>Status</Th><Th>Product</Th><Th>Price</Th><Th>Renews</Th><Th>Expires</Th><Th>Mode</Th></Table.Row></Table.Header>
          <Table.Body>
            {rows.map((p) => (
              <Table.Row key={p.id} className={ROW}>
                <Table.Cell className="py-2 pr-3 font-mono text-xs">{p.purchase_id}</Table.Cell><Table.Cell>{p.provider}</Table.Cell><Table.Cell><SubjectLink subject={p.subject} /></Table.Cell>
                <Table.Cell><PurchaseBadge status={p.status} /></Table.Cell><Table.Cell className="font-mono text-xs">{p.product_id}</Table.Cell><Table.Cell className="font-mono text-xs">{p.price}</Table.Cell>
                <Table.Cell>{p.kind === "subscription" ? (p.auto_renew ? "auto-renews" : "ends") : "one-time"}</Table.Cell><Table.Cell className="text-muted-foreground">{p.expires_at ? when_(p.expires_at) : "-"}</Table.Cell>
                <Table.Cell>{p.environment}</Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
    </Panel>
  );
}

export default function Stores() {
  return (
    <div className="space-y-4">
      <p className="text-sm text-muted-foreground">App Store, Google Play and Creem are merchants of record: the store takes the payment, ForgeBase verifies and mirrors it. Credentials are set under Providers and settings.</p>
      <StoreProducts />
      <StorePurchases />
    </div>
  );
}
