import type { ButtonHTMLAttributes, ReactNode } from "react";
import { useState } from "react";
import { ApiError } from "../api";

const BTN = "rounded-md border px-3 py-1.5 text-sm disabled:opacity-50";
export function Button({ tone = "default", className = "", ...p }: ButtonHTMLAttributes<HTMLButtonElement> & { tone?: "default" | "primary" | "danger" }) {
  const c =
    tone === "primary" ? "border-amber-500 bg-amber-500 text-zinc-950 hover:bg-amber-400"
    : tone === "danger" ? "border-red-700 text-red-300 hover:bg-red-950"
    : "border-zinc-700 text-zinc-200 hover:bg-zinc-800";
  return <button type="button" {...p} className={`${BTN} ${c} ${className}`} />;
}

/** Two-step destructive button: the first click arms it, the second runs `onConfirm`. */
export function ConfirmButton({ label, onConfirm, disabled }: { label: string; onConfirm: () => void; disabled?: boolean }) {
  const [armed, setArmed] = useState(false);
  return armed ? (
    <Button
      tone="danger"
      onClick={() => {
        setArmed(false);
        onConfirm();
      }}
      onBlur={() => setArmed(false)}
      autoFocus
    >
      Confirm {label.toLowerCase()}
    </Button>
  ) : (
    <Button tone="danger" disabled={disabled} onClick={() => setArmed(true)}>
      {label}
    </Button>
  );
}

export const inputCls = "rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1.5 text-sm text-zinc-100 placeholder:text-zinc-500";

/** Sign-in-required state for 401/403, otherwise the PLAN section 5 envelope with per-field errors. */
export function ErrorView({ error }: { error: unknown }) {
  if (!error) return null;
  if (error instanceof ApiError && error.isAuth) {
    return (
      <div role="alert" className="rounded-lg border border-amber-700/60 bg-amber-500/10 p-5">
        <p className="font-medium text-amber-300">Sign-in required</p>
        <p className="mt-1 text-sm text-zinc-300">
          The server refused this request ({error.status} {error.code}). Your account may lack the permission, or the session ended:
          sign in again or ask an administrator.
        </p>
        {error.requestId && <p className="mt-2 font-mono text-xs text-zinc-500">request {error.requestId}</p>}
      </div>
    );
  }
  if (error instanceof ApiError) {
    const fields = Object.entries(error.fields);
    return (
      <div role="alert" className="rounded-lg border border-red-800/60 bg-red-500/10 p-4 text-sm">
        <p className="text-red-300">
          <span className="font-mono">{error.code}</span>: {error.message}
        </p>
        {fields.length > 0 && (
          <ul className="mt-2 list-disc space-y-0.5 pl-5 text-red-200">
            {fields.map(([name, f]) => (
              <li key={name}>
                <span className="font-mono">{name}</span>: {f.message || f.code} <span className="text-red-400/70">({f.code})</span>
              </li>
            ))}
          </ul>
        )}
        {error.requestId && <p className="mt-2 font-mono text-xs text-zinc-500">request {error.requestId}</p>}
      </div>
    );
  }
  return (
    <p role="alert" className="rounded-lg border border-red-800/60 bg-red-500/10 p-4 text-sm text-red-300">
      {error instanceof Error ? error.message : String(error)}
    </p>
  );
}

export function Loading({ what = "Loading" }: { what?: string }) {
  return <p className="text-sm text-zinc-500">{what}…</p>;
}

export function Panel({ title, actions, children }: { title?: string; actions?: ReactNode; children: ReactNode }) {
  return (
    <section className="rounded-lg border border-zinc-800 bg-zinc-900/60 p-4">
      {(title || actions) && (
        <div className="mb-3 flex items-center justify-between gap-2">
          {title && <h2 className="text-sm font-medium text-zinc-300">{title}</h2>}
          {actions}
        </div>
      )}
      {children}
    </section>
  );
}

export function PlanView({ plan }: { plan: { steps: { id: number; parent: number; detail: string }[]; indexes_used: string[]; full_scans: string[] } }) {
  return (
    <div aria-label="Query plan" className="space-y-2 text-sm">
      <ul className="font-mono text-xs text-zinc-300">
        {plan.steps.map((s) => (
          <li key={s.id}>{s.detail}</li>
        ))}
      </ul>
      <p className="text-zinc-400">
        Indexes used: {plan.indexes_used.length ? plan.indexes_used.join(", ") : "none"}
      </p>
      {plan.full_scans.length > 0 && <p className="text-amber-300">Full scans: {plan.full_scans.join(", ")}</p>}
    </div>
  );
}
