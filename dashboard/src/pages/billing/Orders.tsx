import { useState } from "react";
import { createOrder, getInvoice, listOrders, listPrices } from "../../api";
import type { Order } from "../../api";
import { Button, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { Badge, BillingError, Empty, Field, FORM_GRID, Money, ROW, TABLE, THEAD, Th, idempotencyKey, useAct, when_ } from "./shared";

function OrderTotal({ order }: { order: Order }) {
  const inv = useLoad((s) => (order.invoice_id ? getInvoice(order.invoice_id, s) : Promise.resolve(undefined)), `order-inv:${order.id}`);
  return inv.data ? <Money amount={inv.data.total} currency={inv.data.currency} /> : <span className="text-zinc-500">-</span>;
}

export default function Orders() {
  const [subject, setSubject] = useState("");
  const [shown, setShown] = useState("");
  const orders = useLoad((s) => listOrders(shown || undefined, s), `orders:${shown}`);
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
      <p className="text-sm text-zinc-400">One-time purchases: credit top-ups, usage packs and services. Orders are listed per subject.</p>
      <form className="flex items-end gap-2 text-sm" onSubmit={(e) => { e.preventDefault(); setShown(subject); }}>
        <label>Order subject<input className={`${inputCls} ml-2 w-56`} value={subject} onChange={(e) => setSubject(e.target.value)} placeholder="org:acme or a user key" /></label>
        <Button type="submit">Show orders</Button>
      </form>
      <BillingError error={orders.error} />
      {orders.loading && !orders.error && <Loading what="Loading orders" />}
      {orders.data && orders.data.length === 0 && <Empty>No orders for this subject.</Empty>}
      {orders.data && orders.data.length > 0 && (
        <table aria-label="Orders" className={TABLE}>
          <thead className={THEAD}><tr><Th>Order</Th><Th>Subject</Th><Th>Status</Th><Th>Total</Th><Th>Invoice</Th><Th>Created</Th></tr></thead>
          <tbody>
            {orders.data.map((o) => (
              <tr key={o.id} className={ROW}>
                <td className="py-2 pr-3 font-mono text-xs">{o.id}</td><td className="font-mono text-xs">{o.subject}</td><td><Badge status={o.status} /></td>
                <td><OrderTotal order={o} /></td><td className="font-mono text-xs">{o.invoice_id ?? "-"}</td><td className="text-zinc-400">{when_(o.created_at)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <Panel title="New one-time order">
        <form onSubmit={submit} aria-label="New order" className="space-y-3">
          <div className={FORM_GRID}>
            <Field label="One-time price"><select className={inputCls} value={price} onChange={(e) => setPrice(e.target.value)} required><option value="">choose…</option>{(prices.data ?? []).filter((p) => p.type !== "recurring").map((p) => <option key={p.key} value={p.key}>{p.key}</option>)}</select></Field>
            <Field label="Quantity"><input className={inputCls} inputMode="numeric" value={qty} onChange={(e) => setQty(e.target.value)} /></Field>
            <Field label="Coupon"><input className={inputCls} value={coupon} onChange={(e) => setCoupon(e.target.value)} /></Field>
          </div>
          <p className="text-xs text-zinc-500">The order is placed for the subject shown above (blank: the signed-in admin).</p>
          <ErrorView error={act.error} />
          {act.note && <p role="status" className="text-sm text-emerald-300">{act.note}</p>}
          <Button type="submit" tone="primary" disabled={act.busy}>Create order</Button>
        </form>
      </Panel>
    </div>
  );
}
