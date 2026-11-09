import type { ReactNode } from "react";
import { useState } from "react";
import { Button, ErrorView, inputCls } from "../../components/ui";

export const fmtTime = (v: string | number | null | undefined): string => {
  if (v === null || v === undefined || v === "") return "—";
  const d = typeof v === "number" ? new Date(v * 1000) : new Date(v);
  return Number.isNaN(d.getTime()) ? String(v) : d.toISOString().replace("T", " ").slice(0, 19) + "Z";
};

export function Badge({ tone = "default", children }: { tone?: "default" | "ok" | "warn" | "bad"; children: ReactNode }) {
  const c =
    tone === "ok" ? "border-emerald-700 text-emerald-300"
    : tone === "warn" ? "border-amber-600 text-amber-300"
    : tone === "bad" ? "border-red-700 text-red-300"
    : "border-zinc-600 text-zinc-300";
  return <span className={`inline-block rounded border px-1.5 py-0.5 text-xs ${c}`}>{children}</span>;
}

export function Modal({ title, onClose, children }: { title: string; onClose?: () => void; children: ReactNode }) {
  return (
    <div role="dialog" aria-modal="true" aria-label={title} className="fixed inset-0 z-30 flex items-center justify-center bg-black/60 p-4">
      <div className="w-full max-w-lg rounded-lg border border-zinc-700 bg-zinc-900 p-5">
        <div className="mb-3 flex items-center justify-between">
          <h2 className="font-medium">{title}</h2>
          {onClose && (
            <Button aria-label="Close dialog" onClick={onClose}>
              ×
            </Button>
          )}
        </div>
        {children}
      </div>
    </div>
  );
}

/** Shows a secret exactly once; the parent drops it when the dialog closes. */
export function SecretModal({ secret, label, onClose }: { secret: string; label: string; onClose: () => void }) {
  const [copied, setCopied] = useState(false);
  return (
    <Modal title={`${label} created`}>
      <p role="note" className="mb-2 rounded border border-amber-700/60 bg-amber-500/10 p-2 text-sm text-amber-200">
        Copy this secret now. It is shown once and cannot be retrieved later.
      </p>
      <code aria-label="Secret" className="block break-all rounded bg-zinc-950 p-2 font-mono text-xs">
        {secret}
      </code>
      <div className="mt-3 flex justify-end gap-2">
        <Button
          onClick={() => {
            void navigator.clipboard?.writeText(secret);
            setCopied(true);
          }}
        >
          {copied ? "Copied" : "Copy"}
        </Button>
        <Button tone="primary" onClick={onClose}>
          Done
        </Button>
      </div>
    </Modal>
  );
}

/** Runs an async action, keeping its error for display. */
export function useAction() {
  const [error, setError] = useState<unknown>();
  const [busy, setBusy] = useState(false);
  const run = async (fn: () => Promise<unknown>): Promise<boolean> => {
    setBusy(true);
    setError(undefined);
    try {
      await fn();
      return true;
    } catch (e) {
      setError(e);
      return false;
    } finally {
      setBusy(false);
    }
  };
  return { error, busy, run, errorView: <ErrorView error={error} /> };
}

export const splitList = (s: string) => s.split(/[\s,]+/).filter(Boolean);

export function Th({ children }: { children: ReactNode }) {
  return <th className="py-2 pr-3">{children}</th>;
}
export const THEAD = "text-left text-xs uppercase tracking-wide text-zinc-500";
export { inputCls };
