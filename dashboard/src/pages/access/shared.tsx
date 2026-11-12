import { Alert02Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import type { ReactNode } from "react";
import { Badge } from "panelui-web/badge";
import { Input } from "panelui-web/input";
import { ApiError } from "../../api";
import { Check } from "../../components/controls";
import { ErrorView } from "../../components/ui";

/** A label above a control; the label wraps it, so the control is named by the words. Help sits under it, outside the label. */
export function Field({ label, help, className = "", children }: { label: ReactNode; help?: ReactNode; className?: string; children: ReactNode }) {
  return (
    <div className={`flex flex-col gap-1 ${className}`.trim()}>
      <label className="flex flex-col gap-1.5 text-xs font-medium text-foreground/80">
        {label}
        {children}
      </label>
      {help && <span className="text-xs text-muted-foreground">{help}</span>}
    </div>
  );
}

export function TextField({ label, value, onChange, placeholder, help, mono, className = "" }: { label: string; value: string; onChange: (v: string) => void; placeholder?: string; help?: ReactNode; mono?: boolean; className?: string }) {
  return (
    <Field label={label} help={help} className={className}>
      <Input size="sm" className={mono ? "font-mono" : undefined} value={value} placeholder={placeholder} autoComplete="off" spellCheck={mono ? false : undefined} onChange={(e) => onChange(e.target.value)} />
    </Field>
  );
}

/**
 * Checkboxes choosing several keys out of `options`, drawn as a wrap of small bordered options. Each checkbox is named
 * by its key with `aria-label` (the visible key beside it toggles it too), so `getByLabelText(key)` finds one control.
 */
export function MultiPick({ label, options, value, onChange, empty = "None available yet." }: { label: string; options: string[]; value: string[]; onChange: (v: string[]) => void; empty?: ReactNode }) {
  const set = (o: string, on: boolean) => onChange(on ? [...value, o] : value.filter((x) => x !== o));
  return (
    <fieldset className="space-y-1.5">
      <legend className="mb-1.5 text-xs font-medium text-foreground/80">
        {label}
        {value.length > 0 && <span className="ms-1.5 font-normal text-muted-foreground">({value.length} picked)</span>}
      </legend>
      {options.length === 0 ? (
        <p className="text-xs text-muted-foreground">{empty}</p>
      ) : (
        <div className="flex flex-wrap gap-1.5">
          {options.map((o) => {
            const on = value.includes(o);
            return (
              <span
                key={o}
                onClick={(e) => { if (!(e.target as HTMLElement).closest("[role=checkbox]")) set(o, !on); }}
                className={`inline-flex cursor-pointer select-none items-center gap-2 rounded-md border px-2 py-1 transition-colors ${on ? "border-primary/40 bg-primary/5" : "border-border hover:border-foreground/25"}`}
              >
                <Check checked={on} onChange={(v) => set(o, v)} aria-label={o} />
                <span aria-hidden="true" className="font-mono text-xs">{o}</span>
              </span>
            );
          })}
        </div>
      )}
    </fieldset>
  );
}

export function EffectBadge({ effect }: { effect: "allow" | "deny" }) {
  return (
    <Badge data-effect={effect} variant={effect === "deny" ? "destructive" : "success"} size="sm" dot>
      {effect === "deny" ? "Deny" : "Allow"}
    </Badge>
  );
}

/** A principal such as `user:users/abc`, in mono, cut to fit with the whole value on hover. */
export function Principal({ value }: { value: string }) {
  return (
    <span className="block max-w-full truncate font-mono text-xs" title={value}>
      {value}
    </span>
  );
}

/** A card holding a small inline form: a title, one sentence, the fields in a row and the submit button. */
export function FormCard({ title, description, children, footer }: { title: ReactNode; description?: ReactNode; children: ReactNode; footer?: ReactNode }) {
  return (
    <section className="rounded-xl border border-border bg-card">
      <div className="border-b border-border px-4 py-3">
        <h3 className="text-sm font-semibold">{title}</h3>
        {description && <p className="mt-0.5 text-xs text-muted-foreground">{description}</p>}
      </div>
      <div className="space-y-4 px-4 py-4">{children}</div>
      {footer && <div className="flex flex-wrap items-center justify-end gap-2 border-t border-border px-4 py-3">{footer}</div>}
    </section>
  );
}

/** Error of a mutation: a 403 is shown with the server's reason (escalation guard, missing permission), not as "sign in". */
export function ActionError({ error }: { error: unknown }) {
  if (error instanceof ApiError && error.status === 403) {
    const step = (error.body as { error?: { details?: { step?: string } } } | undefined)?.error?.details?.step;
    return (
      <div role="alert" className="flex items-start gap-3 rounded-xl border border-destructive/40 bg-destructive/5 p-4 text-sm">
        <HugeiconsIcon icon={Alert02Icon} size={18} className="mt-0.5 shrink-0 text-destructive" aria-hidden="true" />
        <div className="min-w-0">
          <p className="font-medium text-destructive">The server refused this change</p>
          <p className="mt-0.5 text-foreground/90">{error.message}</p>
          <p className="mt-1 text-xs text-muted-foreground">
            <span className="font-mono">{error.code}</span>
            {step && <> · denied at step {step}</>}
          </p>
        </div>
      </div>
    );
  }
  return <ErrorView error={error} />;
}
