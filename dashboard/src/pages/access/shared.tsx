import type { ReactNode } from "react";
import { ApiError } from "../../api";
import { ErrorView } from "../../components/ui";
import { Input } from "panelui-web/input";
import { Badge } from "panelui-web/badge";

export function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <label className="flex flex-col gap-1 text-xs text-muted-foreground">
      {label}
      {children}
    </label>
  );
}

export function TextField({ label, value, onChange, placeholder }: { label: string; value: string; onChange: (v: string) => void; placeholder?: string }) {
  return (
    <Field label={label}>
      <Input size="sm" value={value} placeholder={placeholder} onChange={(e) => onChange(e.target.value)} />
    </Field>
  );
}

/** Checkbox list choosing several keys out of `options`. */
export function MultiPick({ label, options, value, onChange }: { label: string; options: string[]; value: string[]; onChange: (v: string[]) => void }) {
  return (
    <fieldset className="text-xs text-muted-foreground">
      <legend className="mb-1">{label}</legend>
      <div className="flex flex-wrap gap-x-3 gap-y-1">
        {options.length === 0 && <span className="text-muted-foreground">none available</span>}
        {options.map((o) => (
          <label key={o} className="flex items-center gap-1 text-foreground/80">
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


export function EffectBadge({ effect }: { effect: "allow" | "deny" }) {
  return (
    <Badge data-effect={effect} variant={effect === "deny" ? "destructive" : "success"} size="sm">
      {effect}
    </Badge>
  );
}


/** Error of a mutation: a 403 is shown with the server's reason (escalation guard, missing permission), not as "sign in". */
export function ActionError({ error }: { error: unknown }) {
  if (error instanceof ApiError && error.status === 403) {
    const step = (error.body as { error?: { details?: { step?: string } } } | undefined)?.error?.details?.step;
    return (
      <div role="alert" className="rounded-lg border border-destructive/40 bg-destructive/10 p-4 text-sm text-destructive">
        <span className="font-mono">{error.code}</span>: {error.message}
        {step && <span className="ml-2 text-destructive">(denied at step {step})</span>}
      </div>
    );
  }
  return <ErrorView error={error} />;
}
