import type { ReactNode } from "react";
import { Link } from "react-router-dom";
import { isBillingDisabled } from "../../api";
import { ErrorView } from "../../components/ui";
import { formatMoney } from "./money";
import { Table } from "panelui-web/table";
import { Badge as PanelBadge, type BadgeVariant } from "panelui-web/badge";

const TONES = {
  good: "success",
  warn: "warning",
  bad: "destructive",
  info: "info",
  idle: "secondary",
} as const satisfies Record<string, BadgeVariant>;

const STATUS_TONE: Record<string, keyof typeof TONES> = {
  active: "good", paid: "good", succeeded: "good", trialing: "info", open: "info", pending: "info", incomplete: "warn",
  past_due: "bad", failed: "bad", uncollectible: "bad", paused: "warn", canceled: "idle", void: "idle", draft: "idle", refunded: "warn",
};

export function Badge({ status, label }: { status: string; label?: string }) {
  return (
    <PanelBadge variant={TONES[STATUS_TONE[status] ?? "idle"]} size="sm">
      {label ?? status}
    </PanelBadge>
  );
}

export const Money = ({ amount, currency }: { amount: number; currency: string }) => <span className="tabular-nums">{formatMoney(amount, currency)}</span>;

export const Th = ({ children }: { children?: ReactNode }) => <Table.Head className="py-2 pr-3">{children}</Table.Head>;

/** The billing routes answer 404 while billing is off: say so instead of showing a raw error. */
export function Disabled() {
  return (
    <div role="status" aria-label="Billing disabled" className="rounded-lg border border-border bg-card p-6">
      <h2 className="text-lg font-medium">Billing is disabled on this server</h2>
      <p className="mt-2 text-sm text-muted-foreground">
        Billing is on by default, so it was turned off here (<code className="rounded bg-muted px-1">[billing] enabled = false</code> or <code className="rounded bg-muted px-1">FORGEBASE_BILLING_ENABLED=false</code>),
        or this server is an older build without it. Set <code className="rounded bg-muted px-1">[billing] enabled = true</code> (or remove the setting) and restart to turn it on.
      </p>
    </div>
  );
}

/** Error view that turns a 404 on a billing route into the disabled state. */
export function BillingError({ error }: { error: unknown }) {
  if (!error) return null;
  if (isBillingDisabled(error)) return <Disabled />;
  return <ErrorView error={error} />;
}

export function SubjectLink({ subject }: { subject: string }) {
  return (
  <Link className="font-mono text-xs text-primary hover:underline" to={`/billing/customers?subject=${encodeURIComponent(subject)}`}>{subject}</Link>
  );
}


export function Empty({ children }: { children: ReactNode }) {
  return <p className="text-sm text-muted-foreground">{children}</p>;
}


export function Field({ label, children, className = "" }: { label: string; children: ReactNode; className?: string }) {
  return <label className={`block text-sm text-foreground/80 ${className}`}>{label}<span className="mt-1 block">{children}</span></label>;
}

