import { useState } from "react";
import { EmptyState } from "../../components/EmptyState";
import { archiveCoupon, listCoupons, putCoupon } from "../../api";
import type { CouponDuration, CouponKind } from "../../api";
import { Button, ConfirmButton, ErrorView } from "../../components/ui";
import { Sel } from "../../components/controls";
import { CreateSheet } from "../../components/CreateSheet";
import { FormField } from "../../components/FormField";
import { SkeletonTable } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import { csv } from "../functions/util";
import { formatBp, formatCount, formatMoney, parseMinor } from "./money";
import { BillingError, FormSection, None, Note, TableCard } from "./shared";
import { FORM_GRID } from "./util";
import { useAct } from "./useAct";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { Badge } from "../../components/StatusBadge";
import { IdText } from "../../components/IdText";
import { Th } from "../../components/TableCard";
import { When } from "../../components/Time";

const PERCENT = /^(\d+)(?:\.(\d{1,2}))?$/;

function CouponSheet({ open, onOpenChange, onSaved }: { open: boolean; onOpenChange: (v: boolean) => void; onSaved: () => void }) {
  const [code, setCode] = useState("");
  const [kind, setKind] = useState<CouponKind>("percent");
  const [value, setValue] = useState("");
  const [currency, setCurrency] = useState("USD");
  const [duration, setDuration] = useState<CouponDuration>("once");
  const [cycles, setCycles] = useState("");
  const [max, setMax] = useState("");
  const [products, setProducts] = useState("");
  const cur = currency.trim().toUpperCase();
  const valueError =
    value === "" ? undefined
      : kind === "percent" ? (PERCENT.test(value.trim()) ? undefined : "Percent must look like 15 or 12.5")
        : parseMinor(value, cur) === null ? `${value} is not a valid ${cur} amount` : undefined;
  const whole = (s: string) => s === "" || /^\d+$/.test(s.trim());
  return (
    <CreateSheet
      open={open}
      onOpenChange={onOpenChange}
      title="New coupon"
      description="A coupon takes a percentage or an amount off when a customer enters its code."
      submitLabel="Save coupon"
      canSubmit={!!code.trim() && value !== "" && !valueError && whole(cycles) && whole(max)}
      onSubmit={async () => {
        let percent_bp: number | null = null;
        let amount_off: number | null = null;
        if (kind === "percent") {
          const m = PERCENT.exec(value.trim())!;
          percent_bp = Number(m[1]) * 100 + Number((m[2] ?? "").padEnd(2, "0") || 0);
        } else {
          amount_off = parseMinor(value, cur);
        }
        await putCoupon(code.trim(), {
          kind, percent_bp, amount_off, currency: kind === "amount" ? cur : null, duration, duration_cycles: duration === "repeating" ? Number(cycles) || null : null,
          max_redemptions: max ? Number(max) : null, products: csv(products), active: true,
        });
        onSaved();
        const what = `Coupon ${code.trim()}`;
        setCode(""); setValue(""); setCycles(""); setMax(""); setProducts("");
        return { what };
      }}
    >
      <FormSection title="Code and discount">
        <FormField label="Coupon code" help="What customers type at checkout. Case does not matter.">
          {(p) => <Input {...p} size="sm" className="font-mono uppercase" value={code} onChange={(e) => setCode(e.target.value)} placeholder="SPRING25" />}
        </FormField>
        <div className={FORM_GRID}>
          <FormField label="Coupon kind">
            {(p) => <Sel {...p} value={kind} onChange={(v) => setKind(v as CouponKind)} options={[{ value: "percent", label: "Percent off" }, { value: "amount", label: "Amount off" }]} />}
          </FormField>
          <FormField label={kind === "percent" ? "Percent off" : "Amount off"} error={valueError}>
            {(p) => <Input {...p} size="sm" className="tabular-nums" inputMode="decimal" value={value} onChange={(e) => setValue(e.target.value)} placeholder={kind === "percent" ? "15" : "10.00"} />}
          </FormField>
          {kind === "amount" && (
            <FormField label="Coupon currency">
              {(p) => <Input {...p} size="sm" className="font-mono uppercase" maxLength={3} value={currency} onChange={(e) => setCurrency(e.target.value)} />}
            </FormField>
          )}
        </div>
      </FormSection>
      <FormSection title="How long it applies">
        <div className={FORM_GRID}>
          <FormField label="Duration">
            {(p) => <Sel {...p} value={duration} onChange={(v) => setDuration(v as CouponDuration)} options={[{ value: "once", label: "Once" }, { value: "repeating", label: "Several billing cycles" }, { value: "forever", label: "Forever" }]} />}
          </FormField>
          {duration === "repeating" && (
            <FormField label="Cycles" error={whole(cycles) ? undefined : "A whole number"}>
              {(p) => <Input {...p} size="sm" inputMode="numeric" value={cycles} onChange={(e) => setCycles(e.target.value)} placeholder="3" />}
            </FormField>
          )}
        </div>
      </FormSection>
      <FormSection title="Limits">
        <div className={FORM_GRID}>
          <FormField label="Max redemptions" help="Blank for no limit." error={whole(max) ? undefined : "A whole number"}>
            {(p) => <Input {...p} size="sm" inputMode="numeric" value={max} onChange={(e) => setMax(e.target.value)} />}
          </FormField>
          <FormField label="Only for products" help="Product keys, comma separated. Blank for all.">
            {(p) => <Input {...p} size="sm" className="font-mono" value={products} onChange={(e) => setProducts(e.target.value)} placeholder="pro, team" />}
          </FormField>
        </div>
      </FormSection>
    </CreateSheet>
  );
}

export default function Coupons() {
  const coupons = useLoad(listCoupons, "coupons");
  const act = useAct(coupons.reload);
  const [creating, setCreating] = useState(false);
  const list = coupons.data ?? [];
  const add = <Button tone="primary" onClick={() => setCreating(true)}>New coupon</Button>;
  return (
    <div className="space-y-4">
      <BillingError error={coupons.error} />
      <TableCard title="Coupons" count={coupons.data ? list.length : undefined} description="Discount codes customers enter at checkout." actions={add}>
        {coupons.loading && !coupons.error && <SkeletonTable rows={3} columns={6} label="Loading coupons" />}
        {coupons.data && list.length === 0 && (
          <EmptyState title="No coupons yet" why="A coupon gives a discount when a customer enters its code, once, for a few cycles or forever." action={add} />
        )}
        {list.length > 0 && (
          <Table framed aria-label="Coupons">
            <Table.Header><Table.Row><Th>Code</Th><Th>Discount</Th><Th>Duration</Th><Th align="end">Redeemed</Th><Th>Expires</Th><Th>Status</Th><Th><span className="sr-only">Actions</span></Th></Table.Row></Table.Header>
            <Table.Body>
              {list.map((c) => (
                <Table.Row key={c.code}>
                  <Table.Cell><IdText value={c.code} what="coupon code" /></Table.Cell>
                  <Table.Cell className="font-medium tabular-nums">{c.kind === "percent" ? `${formatBp(c.percent_bp)} off` : `${formatMoney(c.amount_off ?? 0, c.currency ?? "USD")} off`}</Table.Cell>
                  <Table.Cell>{c.duration === "repeating" && c.duration_cycles ? `${c.duration_cycles} cycles` : c.duration}</Table.Cell>
                  <Table.Cell align="end">{formatCount(c.redeemed)}{c.max_redemptions ? <span className="text-muted-foreground"> / {formatCount(c.max_redemptions)}</span> : ""}</Table.Cell>
                  <Table.Cell className="text-muted-foreground">{c.expires_at ? <When iso={c.expires_at} day /> : <None />}</Table.Cell>
                  <Table.Cell><Badge status={c.active ? "active" : "archived"} /></Table.Cell>
                  <Table.Cell className="text-right">
                    {c.active && <ConfirmButton label={`Archive coupon ${c.code}`} short="Archive" consequence="Customers can no longer redeem it. Discounts already applied stay." disabled={act.busy} onConfirm={() => void act.run(() => archiveCoupon(c.code), "Coupon archived")} />}
                  </Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
      <ErrorView error={act.error} />
      <Note>{act.note}</Note>
      <CouponSheet open={creating} onOpenChange={setCreating} onSaved={coupons.reload} />
    </div>
  );
}
