import type { ReactNode } from "react";
import { Button } from "../../components/ui";

export function Check({ label, checked, onChange, hint }: { label: string; checked: boolean; onChange: (v: boolean) => void; hint?: string }) {
  return (
    <label className="flex items-center gap-2 text-sm text-zinc-300">
      <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} />
      {label}
      {hint && <span className="text-xs text-zinc-500">{hint}</span>}
    </label>
  );
}

/** A secret the server shows once: copy it now, it cannot be read again. */
export function SecretOnce({ label, secret, onDismiss }: { label: string; secret: string; onDismiss: () => void }) {
  return (
    <div role="status" aria-label={label} className="rounded-lg border border-amber-700/60 bg-amber-500/10 p-4 text-sm">
      <p className="font-medium text-amber-300">{label}</p>
      <p className="mt-1 text-zinc-300">Copy it now. It is shown once and cannot be read again.</p>
      <code data-testid="secret-once" className="mt-2 block break-all rounded bg-zinc-950 p-2 font-mono text-xs text-zinc-100">{secret}</code>
      <Button className="mt-2" onClick={onDismiss}>I have copied it</Button>
    </div>
  );
}

export function Note({ children }: { children: ReactNode }) {
  return <p className="text-xs text-zinc-500">{children}</p>;
}
