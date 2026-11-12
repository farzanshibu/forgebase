import { useState } from "react";
import { Link } from "react-router-dom";
import { EmptyState } from "../../components/EmptyState";
import { createOrder, getInvoice, listAllOrders, listPrices } from "../../api";
import type { Order, OrderStatus, Price } from "../../api";
import { Button } from "../../components/ui";
import { Sel } from "../../components/controls";
import { CreateSheet } from "../../components/CreateSheet";
import { FormField } from "../../components/FormField";
import { Pager } from "../../components/Pager";
import { SkeletonTable } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import { BillingError, Filter, Money, None, SubjectLink, TableCard } from "./shared";
import { FORM_GRID, idempotencyKey, words } from "./util";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { Badge } from "../../components/StatusBadge";
import { IdText } from "../../components/IdText";
import { Th } from "../../components/TableCard";
import { When } from "../../components/Time";

function OrderTotal({ order }: { order: Order }) {
  const inv = useLoad((s) => (order.invoice_id ? getInvoice(order.invoice_id, s) : Promise.resolve(undefined)), `order-inv:${order.id}`);
  return inv.data ? <Money amount={inv.data.total} currency={inv.data.currency} /> : <None />;
}

const PAGE = 50;
const STATUSES: OrderStatus[] = ["open", "paid", "canceled", "refunded"];

function NewOrder({ open, onOpenChange, prices, subject, onDone }: { open: boolean; onOpenChange: (v: boolean) => void; prices: Price[]; subject: string; onDone: () => void }) {
  const [customer, setCustomer] = useState(subject);
  const [price, setPrice] = useState("");
  const [qty, setQty] = useState("1");
  const [coupon, setCoupon] = useState("");
  const qtyBad = !/^[1-9]\d*$/.test(qty.trim());
  return (
    <CreateSheet
      open={open}
      onOpenChange={onOpenChange}
      title="New order"
      description="A one-time purchase: a credit top-up, a usage pack or a service. It is invoiced at once."
      submitLabel="Create order"
      canSubmit={!!price && !qtyBad}
      onSubmit={async () => {
        await createOrder({ subject: customer.trim() || undefined, items: [{ price, quantity: Number(qty) || 1 }], coupon: coupon || null, idempotency_key: idempotencyKey("order") });
        onDone();
        setPrice(""); setQty("1"); setCoupon("");
        return { what: "Order" };
      }}
    >
      <FormField label="Customer" help="org:<id> or a user key. Blank places the order for you, the signed-in administrator.">
        {(p) => <Input {...p} size="sm" className="font-mono" value={customer} onChange={(e) => setCustomer(e.target.value)} placeholder="org:acme" />}
      </FormField>
      <FormField label="One-time price">
        {(p) => <Sel {...p} value={price} onChange={setPrice} placeholder="Choose a price" options={prices.filter((x) => x.type !== "recurring" && x.active).map((x) => ({ value: x.key, label: `${x.key} (${words(x.type)})` }))} />}
      </FormField>
      <div className={FORM_GRID}>
        <FormField label="Quantity" error={qtyBad ? "A whole number, 1 or more" : undefined}>
          {(p) => <Input {...p} size="sm" inputMode="numeric" value={qty} onChange={(e) => setQty(e.target.value)} />}
        </FormField>
        <FormField label="Coupon" help="Optional coupon code.">
          {(p) => <Input {...p} size="sm" className="font-mono uppercase" value={coupon} onChange={(e) => setCoupon(e.target.value)} />}
        </FormField>
      </div>
    </CreateSheet>
  );
}

export default function Orders() {
  const [subject, setSubject] = useState("");
  const [shown, setShown] = useState("");
  const [status, setStatus] = useState<OrderStatus | "">("");
  const [offset, setOffset] = useState(0);
  const [creating, setCreating] = useState(false);
  const orders = useLoad((s) => listAllOrders({ subject: shown || undefined, status: status || undefined, limit: PAGE, offset }, s), `orders:${shown}:${status}:${offset}`);
  const prices = useLoad((s) => listPrices(undefined, s), "order-prices");
  const items = orders.data?.items ?? [];
  const add = <Button tone="primary" onClick={() => setCreating(true)}>New order</Button>;
  const toolbar = (
    <>
      <form className="flex items-center gap-2" onSubmit={(e) => { e.preventDefault(); setOffset(0); setShown(subject.trim()); }}>
        <Filter label="Subject">
          {(id) => <Input id={id} size="sm" className="w-56 font-mono" value={subject} onChange={(e) => setSubject(e.target.value)} placeholder="org:acme or a user key" />}
        </Filter>
        <Button type="submit">Show orders</Button>
      </form>
      <Filter label="Status">
        {(id) => <Sel placeholder="Any status" id={id} value={status} onChange={(v) => { setOffset(0); setStatus(v as OrderStatus | ""); }} options={[{ value: "", label: "Any status" }, ...STATUSES]} className="w-36" />}
      </Filter>
    </>
  );
  return (
    <div className="space-y-4">
      <BillingError error={orders.error} />
      <TableCard
        title="Orders"
        count={orders.data?.total}
        description="One-time purchases: credit top-ups, usage packs and services, newest first."
        actions={add}
        toolbar={toolbar}
        footer={orders.data && orders.data.total > 0 ? <Pager page={Math.floor(offset / PAGE) + 1} perPage={PAGE} total={orders.data.total} onPage={(p) => setOffset((p - 1) * PAGE)} loading={orders.loading} label="Orders paging" /> : undefined}
      >
        {orders.loading && !orders.error && !orders.data && <SkeletonTable rows={4} columns={6} label="Loading orders" />}
        {orders.data && items.length === 0 && (
          orders.data.total === 0 && !shown && !status
            ? <EmptyState title="No orders yet" why="An order is a one-time purchase, such as a credit top-up. It is invoiced and paid once." action={add} />
            : <EmptyState compact title="No orders match" why="Change the subject or status to see more." />
        )}
        {items.length > 0 && (
          <Table framed aria-label="Orders">
            <Table.Header><Table.Row><Th>Order</Th><Th>Customer</Th><Th>Status</Th><Th align="end">Total</Th><Th>Invoice</Th><Th>Created</Th></Table.Row></Table.Header>
            <Table.Body>
              {items.map((o) => (
                <Table.Row key={o.id}>
                  <Table.Cell><IdText value={o.id} what="order id" /></Table.Cell>
                  <Table.Cell><SubjectLink subject={o.subject} /></Table.Cell>
                  <Table.Cell><Badge status={o.status} /></Table.Cell>
                  <Table.Cell align="end"><OrderTotal order={o} /></Table.Cell>
                  <Table.Cell>{o.invoice_id ? <Link className="font-mono text-xs text-primary hover:underline" to={`/billing/invoices/${encodeURIComponent(o.invoice_id)}`}>{o.invoice_id}</Link> : <None />}</Table.Cell>
                  <Table.Cell className="text-muted-foreground"><When iso={o.created_at} /></Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
      <NewOrder key={shown} open={creating} onOpenChange={setCreating} prices={prices.data ?? []} subject={shown} onDone={orders.reload} />
    </div>
  );
}
