import { useState } from "react";
import { Link } from "react-router-dom";
import { EmptyState } from "../../components/EmptyState";
import { deleteStoreProduct, listPrices, listStoreProducts, listStorePurchases, putStoreProduct, STORE_PROVIDERS } from "../../api";
import { Button, ConfirmButton, ErrorView } from "../../components/ui";
import { Sel } from "../../components/controls";
import { CreateSheet } from "../../components/CreateSheet";
import { FormField } from "../../components/FormField";
import { SkeletonTable } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import { notify } from "../../toaster";
import { BillingError, Filter, None, Note, SubjectLink, TableCard, Tag } from "./shared";
import { words } from "./util";
import { useAct } from "./useAct";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { Badge } from "../../components/StatusBadge";
import { IdText } from "../../components/IdText";
import { Th } from "../../components/TableCard";
import { When } from "../../components/Time";

/** Store states that grant access are green; the rest follow the shared tones through this map. */
const PurchaseBadge = ({ status }: { status: string }) => <Badge status={status}>{words(status)}</Badge>;
const STORE_NAMES: Record<string, string> = { app_store: "App Store", google_play: "Google Play", creem: "Creem" };
const storeName = (id: string) => STORE_NAMES[id] ?? id;

type Mapping = { provider: string; product_id: string; price: string };

function StoreProducts() {
  const products = useLoad(listStoreProducts, "store-products");
  const prices = useLoad((s) => listPrices(undefined, s), "store-prices");
  const act = useAct(products.reload);
  const [sheet, setSheet] = useState<"new" | "edit">();
  const [provider, setProvider] = useState<string>(STORE_PROVIDERS[0]!);
  const [productId, setProductId] = useState("");
  const [price, setPrice] = useState("");
  const [filter, setFilter] = useState("");
  const rows = (products.data ?? []).filter((p) => !filter || p.provider === filter);
  const openSheet = (p?: Mapping) => {
    setProvider(p?.provider ?? STORE_PROVIDERS[0]!);
    setProductId(p?.product_id ?? "");
    setPrice(p?.price ?? "");
    setSheet(p ? "edit" : "new");
  };
  const add = <Button tone="primary" onClick={() => openSheet()}>Map a store product</Button>;
  return (
    <>
      <TableCard
        title="Store products"
        count={products.data ? rows.length : undefined}
        description="Each store product id is sold as one price: a recurring price becomes a subscription, a one-time price an order."
        actions={add}
        toolbar={<Filter label="Store">{(id) => <Sel placeholder="All stores" id={id} aria-label="Filter store" value={filter} onChange={setFilter} options={[{ value: "", label: "All stores" }, ...STORE_PROVIDERS.map((s) => ({ value: s, label: storeName(s) }))]} className="w-40" />}</Filter>}
      >
        <BillingError error={products.error} />
        {products.loading && !products.error && <SkeletonTable rows={3} columns={5} label="Loading store products" />}
        {products.data && rows.length === 0 && <EmptyState compact title="No store products mapped" why="Map a store product id to a price so a store purchase grants it: recurring for a store subscription, one-time otherwise." action={filter ? undefined : add} />}
        {rows.length > 0 && (
          <Table framed aria-label="Store products">
            <Table.Header><Table.Row><Th>Store</Th><Th>Store product id</Th><Th>Price</Th><Th>Kind</Th><Th>Changed</Th><Th><span className="sr-only">Actions</span></Th></Table.Row></Table.Header>
            <Table.Body>
              {rows.map((p) => (
                <Table.Row key={`${p.provider}/${p.product_id}`}>
                  <Table.Cell className="whitespace-nowrap">{storeName(p.provider)}</Table.Cell>
                  <Table.Cell><IdText value={p.product_id} what="product id" /></Table.Cell>
                  <Table.Cell className="font-mono text-xs">{p.price}</Table.Cell>
                  <Table.Cell><Tag>{words(p.kind)}</Tag></Table.Cell>
                  <Table.Cell className="text-muted-foreground"><When iso={p.updated_at} day /></Table.Cell>
                  <Table.Cell className="whitespace-nowrap text-right">
                    <span className="inline-flex gap-2">
                      <Button onClick={() => openSheet(p)} aria-label={`Edit ${p.provider} ${p.product_id}`}>Edit</Button>
                      <ConfirmButton label={`Delete ${p.provider} ${p.product_id}`} short="Delete" consequence="New purchases of this store product no longer grant anything. Past purchases stay." disabled={act.busy} onConfirm={() => void act.run(() => deleteStoreProduct(p.provider, p.product_id), "Mapping deleted")} />
                    </span>
                  </Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
      <ErrorView error={act.error} />
      <Note>{act.note}</Note>
      <CreateSheet
        open={!!sheet}
        onOpenChange={(v) => { if (!v) setSheet(undefined); }}
        title={sheet === "edit" ? "Edit mapping" : "Map a store product"}
        description="Saving an existing store product id re-maps it. The price type decides what is delivered."
        submitLabel="Save mapping"
        canSubmit={!!productId.trim() && !!price}
        onSubmit={async () => {
          await putStoreProduct(provider, productId.trim(), price);
          products.reload();
          notify.saved("Mapping");
        }}
      >
        <FormField label="Mapping store">
          {(p) => <Sel {...p} value={provider} onChange={setProvider} disabled={sheet === "edit"} options={STORE_PROVIDERS.map((s) => ({ value: s, label: storeName(s) }))} />}
        </FormField>
        <FormField label="Store product id" help="The product id as the store knows it.">
          {(p) => <Input {...p} size="sm" className="font-mono" value={productId} disabled={sheet === "edit"} onChange={(e) => setProductId(e.target.value)} placeholder="com.example.pro.monthly" />}
        </FormField>
        <FormField label="Sold as price" help="Metered prices cannot be sold through a store.">
          {(p) => <Sel {...p} value={price} onChange={setPrice} placeholder="Choose a price" options={(prices.data ?? []).filter((x) => x.active && x.type !== "metered").map((x) => ({ value: x.key, label: `${x.key} (${words(x.type)})` }))} />}
        </FormField>
      </CreateSheet>
    </>
  );
}

function StorePurchases() {
  const [subject, setSubject] = useState("");
  const [status, setStatus] = useState("");
  const purchases = useLoad((s) => listStorePurchases(subject || undefined, s), `store-purchases:${subject}`);
  const rows = (purchases.data ?? []).filter((p) => !status || p.status === status);
  const toolbar = (
    <>
      <Filter label="Status">
        {(id) => <Sel placeholder="All statuses" id={id} value={status} onChange={setStatus} options={[{ value: "", label: "All statuses" }, ...["active", "trialing", "grace", "on_hold", "paused", "expired", "refunded", "revoked", "pending", "replaced"].map((s) => ({ value: s, label: words(s) }))]} className="w-40" />}
      </Filter>
      <Filter label="Subject">
        {(id) => <Input id={id} size="sm" className="w-56 font-mono" value={subject} onChange={(e) => setSubject(e.target.value)} placeholder="All subjects" />}
      </Filter>
    </>
  );
  return (
    <TableCard title="Store purchases" count={purchases.data ? rows.length : undefined} description="Purchases the stores reported, verified and mirrored here." actions={<Button onClick={purchases.reload} disabled={purchases.loading}>Refresh</Button>} toolbar={toolbar}>
      <BillingError error={purchases.error} />
      {purchases.loading && !purchases.error && <SkeletonTable rows={3} columns={7} label="Loading store purchases" />}
      {purchases.data && rows.length === 0 && <EmptyState compact title="No store purchases match" why="Purchases appear here once a store reports one. Change the filters to see more." />}
      {rows.length > 0 && (
        <Table framed aria-label="Store purchases">
          <Table.Header><Table.Row><Th>Purchase</Th><Th>Store</Th><Th>Customer</Th><Th>Status</Th><Th>Product</Th><Th>Price</Th><Th>Renews</Th><Th>Expires</Th><Th>Mode</Th></Table.Row></Table.Header>
          <Table.Body>
            {rows.map((p) => (
              <Table.Row key={p.id}>
                <Table.Cell><IdText value={p.purchase_id} what="purchase id" /></Table.Cell>
                <Table.Cell className="whitespace-nowrap">{storeName(p.provider)}</Table.Cell>
                <Table.Cell><SubjectLink subject={p.subject} /></Table.Cell>
                <Table.Cell><PurchaseBadge status={p.status} /></Table.Cell>
                <Table.Cell className="max-w-48 truncate font-mono text-xs" title={p.product_id}>{p.product_id}</Table.Cell>
                <Table.Cell className="font-mono text-xs">{p.price}</Table.Cell>
                <Table.Cell className="whitespace-nowrap">{p.kind === "subscription" ? (p.auto_renew ? "auto-renews" : "ends") : "one-time"}</Table.Cell>
                <Table.Cell className="text-muted-foreground">{p.expires_at ? <When iso={p.expires_at} day /> : <None />}</Table.Cell>
                <Table.Cell><Tag>{p.environment}</Tag></Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
    </TableCard>
  );
}

export default function Stores() {
  return (
    <div className="space-y-5">
      <p className="rounded-xl border border-border bg-muted/30 px-4 py-3 text-sm text-muted-foreground">
        App Store, Google Play and Creem are merchants of record: the store takes the payment, ForgeBase verifies and mirrors it. Set their credentials under{" "}
        <Link to="/billing/settings" className="font-medium text-primary underline-offset-2 hover:underline">Settings</Link>.
      </p>
      <StoreProducts />
      <StorePurchases />
    </div>
  );
}
