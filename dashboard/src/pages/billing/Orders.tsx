import { useState } from "react";
import { createOrder, getInvoice, listAllOrders, listPrices } from "../../api";
import type { Order, OrderStatus } from "../../api";
import { Button, ErrorView, Loading, Panel } from "../../components/ui";
import { Sel } from "../../components/controls";
import { useLoad } from "../../hooks";
import { Badge, BillingError, Empty, Field, Money, Th } from "./shared";
import { FORM_GRID, ROW, TABLE, THEAD, idempotencyKey, when_ } from "./util";
import { useAct } from "./useAct";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";

function OrderTotal({ order }: { order: Order }) {
  const inv = useLoad((s) => (order.invoice_id ? getInvoice(order.invoice_id, s) : Promise.resolve(undefined)), `order-inv:${order.id}`);
  return inv.data ? <Money amount={inv.data.total} currency={inv.data.currency} /> : <span className="text-muted-foreground">-</span>;
}

const PAGE = 50;
const STATUSES: OrderStatus[] = ["open", "paid", "canceled", "refunded"];

export default function Orders() {
  const [subject, setSubject] = useState("");
  const [shown, setShown] = useState("");
  const [status, setStatus] = useState<OrderStatus | "">("");
  const [offset, setOffset] = useState(0);
  const orders = useLoad((s) => listAllOrders({ subject: shown || undefined, status: status || undefined, limit: PAGE, offset }, s), `orders:${shown}:${status}:${offset}`);
  const prices = useLoad((s) => listPrices(undefined, s), "order-prices");
  const act = useAct(orders.reload);
  const [price, setPrice] = useState("");
  const [qty, setQty] = useState("1");
  const [coupon, setCoupon] = useState("");
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    await act.run(() => createOrder({ subject: shown || undefined, items: [{ price, quantity: Number(qty) || 1 }], coupon: coupon || null, idempotency_key: idempotencyKey("order") }), "Order created");
  };
  return (
    <div className="space-y-4">
      <p className="text-sm text-muted-foreground">One-time purchases: credit top-ups, usage packs and services. All orders, newest first; filter by subject or status.</p>
      <form className="flex items-end gap-2 text-sm" onSubmit={(e) => { e.preventDefault(); setOffset(0); setShown(subject); }}>
        <label>Order subject<Input size="sm" className="ml-2 w-56" value={subject} onChange={(e) => setSubject(e.target.value)} placeholder="org:acme or a user key" /></label>
        <label>Status<Sel value={status} onChange={(v) => { setOffset(0); setStatus(v as OrderStatus | ""); }} options={[{ value: "", label: "any" }, ...STATUSES]} className="ml-2" /></label>
        <Button type="submit">Show orders</Button>
      </form>
      <BillingError error={orders.error} />
      {orders.loading && !orders.error && <Loading what="Loading orders" />}
      {orders.data && orders.data.items.length === 0 && <Empty>No orders match.</Empty>}
      {orders.data && orders.data.items.length > 0 && (
        <Table aria-label="Orders" className={TABLE}>
          <Table.Header className={THEAD}><Table.Row><Th>Order</Th><Th>Subject</Th><Th>Status</Th><Th>Total</Th><Th>Invoice</Th><Th>Created</Th></Table.Row></Table.Header>
          <Table.Body>
            {orders.data.items.map((o) => (
              <Table.Row key={o.id} className={ROW}>
                <Table.Cell className="py-2 pr-3 font-mono text-xs">{o.id}</Table.Cell><Table.Cell className="font-mono text-xs">{o.subject}</Table.Cell><Table.Cell><Badge status={o.status} /></Table.Cell>
                <Table.Cell><OrderTotal order={o} /></Table.Cell><Table.Cell className="font-mono text-xs">{o.invoice_id ?? "-"}</Table.Cell><Table.Cell className="text-muted-foreground">{when_(o.created_at)}</Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
      {orders.data && orders.data.total > 0 && (
        <div className="flex items-center gap-3 text-sm text-muted-foreground" aria-label="Orders paging">
          <span>{orders.data.offset + 1}-{orders.data.offset + orders.data.items.length} of {orders.data.total}</span>
          <Button disabled={offset === 0} onClick={() => setOffset(Math.max(0, offset - PAGE))}>Previous</Button>
          <Button disabled={offset + PAGE >= orders.data.total} onClick={() => setOffset(offset + PAGE)}>Next</Button>
        </div>
      )}
      <Panel title="New one-time order">
        <form onSubmit={submit} aria-label="New order" className="space-y-3">
          <div className={FORM_GRID}>
            <Field label="One-time price"><Sel value={price} onChange={setPrice} options={[{ value: "", label: "choose…" }, ...(prices.data ?? []).filter((p) => p.type !== "recurring").map((p) => ({ value: p.key, label: p.key }))]} required /></Field>
            <Field label="Quantity"><Input size="sm" inputMode="numeric" value={qty} onChange={(e) => setQty(e.target.value)} /></Field>
            <Field label="Coupon"><Input size="sm" value={coupon} onChange={(e) => setCoupon(e.target.value)} /></Field>
          </div>
          <p className="text-xs text-muted-foreground">The order is placed for the subject shown above (blank: the signed-in admin).</p>
          <ErrorView error={act.error} />
          {act.note && <p role="status" className="text-sm text-success">{act.note}</p>}
          <Button type="submit" tone="primary" disabled={act.busy}>Create order</Button>
        </form>
      </Panel>
    </div>
  );
}
