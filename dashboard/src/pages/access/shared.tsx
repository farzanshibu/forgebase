import type { ReactNode } from "react";
import { ApiError } from "../../api";
import { ErrorView, inputCls } from "../../components/ui";

export function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <label className="flex flex-col gap-1 text-xs text-zinc-400">
      {label}
      {children}
    </label>
  );
}

export function TextField({ label, value, onChange, placeholder }: { label: string; value: string; onChange: (v: string) => void; placeholder?: string }) {
  return (
    <Field label={label}>
      <input className={inputCls} value={value} placeholder={placeholder} onChange={(e) => onChange(e.target.value)} />
    </Field>
  );
}

/** Checkbox list choosing several keys out of `options`. */
export function MultiPick({ label, options, value, onChange }: { label: string; options: string[]; value: string[]; onChange: (v: string[]) => void }) {
  return (
    <fieldset className="text-xs text-zinc-400">
      <legend className="mb-1">{label}</legend>
      <div className="flex flex-wrap gap-x-3 gap-y-1">
        {options.length === 0 && <span className="text-zinc-600">none available</span>}
        {options.map((o) => (
          <label key={o} className="flex items-center gap-1 text-zinc-300">
            <input
              type="checkbox"
              checked={value.includes(o)}
              onChange={(e) => onChange(e.target.checked ? [...value, o] : value.filter((x) => x !== o))}
            />
            {o}
          </label>
        ))}
      </div>
    </fieldset>
  );
}

export function Empty({ children }: { children: ReactNode }) {
  return <p className="text-sm text-zinc-500">{children}</p>;
}

export function EffectBadge({ effect }: { effect: "allow" | "deny" }) {
  return (
    <span
      data-effect={effect}
      className={`rounded px-1.5 py-0.5 text-xs font-medium ${effect === "deny" ? "bg-red-500/20 text-red-300" : "bg-emerald-500/15 text-emerald-300"}`}
    >
      {effect}
    </span>
  );
}


/** Error of a mutation: a 403 is shown with the server's reason (escalation guard, missing permission), not as "sign in". */
export function ActionError({ error }: { error: unknown }) {
  if (error instanceof ApiError && error.status === 403) {
    const step = (error.body as { error?: { details?: { step?: string } } } | undefined)?.error?.details?.step;
    return (
      <div role="alert" className="rounded-lg border border-red-800/60 bg-red-500/10 p-4 text-sm text-red-300">
        <span className="font-mono">{error.code}</span>: {error.message}
        {step && <span className="ml-2 text-red-400/70">(denied at step {step})</span>}
      </div>
    );
  }
  return <ErrorView error={error} />;
}
