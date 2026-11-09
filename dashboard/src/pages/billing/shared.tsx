import { useCallback, useState } from "react";
import type { ReactNode } from "react";
import { Link } from "react-router-dom";
import { isBillingDisabled } from "../../api";
import { ErrorView } from "../../components/ui";
import { when } from "../functions/util";
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

export function Badge({ status }: { status: string }) {
  return <span className={`rounded px-1.5 py-0.5 text-xs ${TONES[STATUS_TONE[status] ?? "idle"]}`}>{status}</span>;
}

export const Money = ({ amount, currency }: { amount: number; currency: string }) => <span className="tabular-nums">{formatMoney(amount, currency)}</span>;

export const Th = ({ children }: { children?: ReactNode }) => <th className="py-2 pr-3">{children}</th>;
export const THEAD = "text-xs uppercase tracking-wide text-zinc-500";
export const TABLE = "w-full text-left text-sm";
export const ROW = "border-t border-zinc-800";
export const when_ = when;

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

export const subjectLink = (subject: string) => (
  <Link className="font-mono text-xs text-amber-300 hover:underline" to={`/billing/customers?subject=${encodeURIComponent(subject)}`}>{subject}</Link>
);

export const idempotencyKey = (prefix: string) => `${prefix}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;

export function Empty({ children }: { children: ReactNode }) {
  return <p className="text-sm text-zinc-400">{children}</p>;
}


/** Run a mutation: tracks busy and the last error, then calls `after` (usually a reload) on success. */
export function useAct(after?: () => void) {
  const [error, setError] = useState<unknown>();
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string>();
  const run = useCallback(
    async <T,>(fn: () => Promise<T>, message?: string): Promise<T | undefined> => {
      setBusy(true);
      setError(undefined);
      setNote(undefined);
      try {
        const v = await fn();
        if (message) setNote(message);
        after?.();
        return v;
      } catch (e) {
        setError(e);
        return undefined;
      } finally {
        setBusy(false);
      }
    },
    [after],
  );
  return { run, error, busy, note };
}

export function Field({ label, children, className = "" }: { label: string; children: ReactNode; className?: string }) {
  return <label className={`block text-sm text-zinc-300 ${className}`}>{label}<span className="mt-1 block">{children}</span></label>;
}

export const FORM_GRID = "grid gap-3 sm:grid-cols-2 lg:grid-cols-3";
export const parseIntStrict = (t: string): number | null => (/^-?\d+$/.test(t.trim()) ? Number(t.trim()) : null);
