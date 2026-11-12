/**
 * Identity's form pieces. Badges, ids, column headings and ConfirmButton are the shared components (src/components);
 * the table card, tags and form sections are Billing's (`../billing/shared`), so the two studios read alike.
 *
 * Usage:
 *   <Field label="Client id" help="From the provider's console."><Input … /></Field>
 *   <MultiPick label="Scopes" options={OIDC_SCOPES} value={scopes} onChange={setScopes} />
 *   <Check label="Skip consent screen" hint="First-party apps only" checked={skip} onChange={setSkip} />
 */
import type { ReactNode } from "react";
import { Input } from "panelui-web/input";
import { Check as PanelCheck } from "../../components/controls";
import { Button, CopyButton } from "../../components/ui";

/** A checkbox with its words; `hint` is a quieter line under them. */
export function Check({ label, checked, onChange, hint }: { label: string; checked: boolean; onChange: (v: boolean) => void; hint?: string }) {
  return <PanelCheck checked={checked} onChange={onChange} label={label} description={hint} />;
}

/** A labelled control; `help` sits under it, outside the label, so the label stays the control's whole name. */
export function Field({ label, help, children, className = "" }: { label: string; help?: ReactNode; children: ReactNode; className?: string }) {
  return (
    <div className={`flex min-w-0 flex-col gap-1.5 ${className}`.trim()}>
      <label className="flex flex-col gap-1.5 text-sm font-medium text-foreground">
        {label}
        <span className="font-normal">{children}</span>
      </label>
      {help && <p className="text-xs text-muted-foreground">{help}</p>}
    </div>
  );
}

export function TextField({ label, value, onChange, placeholder, help, mono, className }: { label: string; value: string; onChange: (v: string) => void; placeholder?: string; help?: ReactNode; mono?: boolean; className?: string }) {
  return (
    <Field label={label} help={help} className={className}>
      <Input size="sm" className={mono ? "font-mono" : undefined} value={value} placeholder={placeholder} onChange={(e) => onChange(e.target.value)} />
    </Field>
  );
}

/** Several keys out of `options`, as a grid of checkboxes. */
export function MultiPick({ label, options, value, onChange, help }: { label: string; options: string[]; value: string[]; onChange: (v: string[]) => void; help?: ReactNode }) {
  return (
    <fieldset className="min-w-0 space-y-2">
      <legend className="text-sm font-medium">{label}</legend>
      {help && <p className="-mt-1 text-xs text-muted-foreground">{help}</p>}
      <div className="grid grid-cols-1 gap-x-4 gap-y-2 rounded-lg border border-border p-3 sm:grid-cols-2">
        {options.length === 0 && <span className="text-sm text-muted-foreground">None available</span>}
        {options.map((o) => (
          <PanelCheck key={o} checked={value.includes(o)} onChange={(on) => onChange(on ? [...value, o] : value.filter((x) => x !== o))} label={<span className="font-mono text-xs">{o}</span>} aria-label={o} />
        ))}
      </div>
    </fieldset>
  );
}

/** A secret the server shows once: copy it now, it cannot be read again. */
export function SecretOnce({ label, secret, onDismiss }: { label: string; secret: string; onDismiss: () => void }) {
  return (
    <div role="status" aria-label={label} className="rounded-xl border border-warning/40 bg-warning/10 p-4 text-sm">
      <p className="font-semibold text-warning">{label}</p>
      <p className="mt-1 text-foreground/80">Copy it now. It is shown once and cannot be read again.</p>
      <div className="mt-3 flex flex-wrap items-center gap-2">
        <code data-testid="secret-once" className="min-w-0 flex-1 break-all rounded-md border border-border bg-background px-3 py-2 font-mono text-xs text-foreground">{secret}</code>
        <CopyButton text={secret} label="Copy secret" />
      </div>
      <Button className="mt-3" onClick={onDismiss}>I have copied it</Button>
    </div>
  );
}

export function Note({ children }: { children: ReactNode }) {
  return <p className="text-xs text-muted-foreground">{children}</p>;
}

/** The quiet "Saved" line next to a settings form's button. */
export function Saved({ show }: { show: boolean }) {
  if (!show) return null;
  return <span role="status" className="text-sm text-success">Saved</span>;
}
