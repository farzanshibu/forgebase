import type { ReactNode } from "react";
import { Link } from "react-router-dom";
import { isBillingDisabled } from "../../api";
import { ErrorView } from "../../components/ui";
import { formatMoney } from "./money";

const TONES = {
  good: "bg-emerald-500/20 text-emerald-300",
  warn: "bg-amber-500/20 text-amber-300",
  bad: "bg-red-500/20 text-red-300",
  info: "bg-sky-500/20 text-sky-300",
  idle: "bg-zinc-500/20 text-zinc-300",
} as const;

const STATUS_TONE: Record<string, keyof typeof TONES> = {
  active: "good", paid: "good", succeeded: "good", trialing: "info", open: "info", pending: "info", incomplete: "warn",
  past_due: "bad", failed: "bad", uncollectible: "bad", paused: "warn", canceled: "idle", void: "idle", draft: "idle", refunded: "warn",
};

export function Badge({ status, label }: { status: string; label?: string }) {
  return <span className={`rounded px-1.5 py-0.5 text-xs ${TONES[STATUS_TONE[status] ?? "idle"]}`}>{label ?? status}</span>;
}

export const Money = ({ amount, currency }: { amount: number; currency: string }) => <span className="tabular-nums">{formatMoney(amount, currency)}</span>;

export const Th = ({ children }: { children?: ReactNode }) => <th className="py-2 pr-3">{children}</th>;

/** The billing routes answer 404 while billing is off (the default): say so instead of showing a raw error. */
export function Disabled() {
  return (
    <div role="status" aria-label="Billing disabled" className="rounded-lg border border-zinc-700 bg-zinc-900/60 p-6">
      <h2 className="text-lg font-medium">Billing is disabled on this server</h2>
      <p className="mt-2 text-sm text-zinc-400">
        The billing API is off by default. Set <code className="rounded bg-zinc-800 px-1">[billing] enabled = true</code> in the server configuration
        (or <code className="rounded bg-zinc-800 px-1">FORGEBASE_BILLING_ENABLED=true</code>) and restart to turn it on.
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
  <Link className="font-mono text-xs text-amber-300 hover:underline" to={`/billing/customers?subject=${encodeURIComponent(subject)}`}>{subject}</Link>
  );
}


export function Empty({ children }: { children: ReactNode }) {
  return <p className="text-sm text-zinc-400">{children}</p>;
}


export function Field({ label, children, className = "" }: { label: string; children: ReactNode; className?: string }) {
  return <label className={`block text-sm text-zinc-300 ${className}`}>{label}<span className="mt-1 block">{children}</span></label>;
}

