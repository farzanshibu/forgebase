import type { ReactNode } from "react";
import { useState } from "react";
import { Button, ErrorView } from "../../components/ui";
import { Dialog } from "panelui-web/dialog";
import { Table } from "panelui-web/table";
import { toaster } from "../../toaster";

export const fmtTime = (v: string | number | null | undefined): string => {
  if (v === null || v === undefined || v === "") return "—";
  const d = typeof v === "number" ? new Date(v * 1000) : new Date(v);
  return Number.isNaN(d.getTime()) ? String(v) : d.toISOString().replace("T", " ").slice(0, 19) + "Z";
};

export function Badge({ tone = "default", children }: { tone?: "default" | "ok" | "warn" | "bad"; children: ReactNode }) {
  const c =
    tone === "ok" ? "border-success/40 text-success"
    : tone === "warn" ? "border-warning/40 text-warning"
    : tone === "bad" ? "border-destructive/40 text-destructive"
    : "border-foreground/25 text-foreground/80";
  return <span className={`inline-block rounded border px-1.5 py-0.5 text-xs ${c}`}>{children}</span>;
}

/** PanelUI dialog, open while mounted. Without `onClose` it can only be dismissed by what the children offer (a secret shown once). */
export function Modal({ title, onClose, footer, children }: { title: string; onClose?: () => void; footer?: ReactNode; children: ReactNode }) {
  return (
    <Dialog open onOpenChange={(open) => { if (!open) onClose?.(); }} dismissible={false}>
      <Dialog.Content size="md" showClose={Boolean(onClose)}>
        <Dialog.Header title={title} />
        <Dialog.Body>{children}</Dialog.Body>
        {footer && <Dialog.Footer>{footer}</Dialog.Footer>}
      </Dialog.Content>
    </Dialog>
  );
}

/** Shows a secret exactly once; the parent drops it when the dialog closes. */
export function SecretModal({ secret, label, onClose }: { secret: string; label: string; onClose: () => void }) {
  const [copied, setCopied] = useState(false);
  return (
    <Modal
      title={`${label} created`}
      footer={
        <>
          <Button
            onClick={() => {
              void navigator.clipboard?.writeText(secret);
              setCopied(true);
              toaster.add({ title: "Secret copied", description: "It is on your clipboard. Store it somewhere safe.", type: "success" });
            }}
          >
            {copied ? "Copied" : "Copy"}
          </Button>
          <Button tone="primary" onClick={onClose}>
            Done
          </Button>
        </>
      }
    >
      <p role="note" className="mb-2 rounded border border-warning/40 bg-warning/10 p-2 text-sm text-warning">
        Copy this secret now. It is shown once and cannot be retrieved later.
      </p>
      <code aria-label="Secret" className="block break-all rounded bg-background p-2 font-mono text-xs">
        {secret}
      </code>
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
  return <Table.Head className="py-2 pr-3">{children}</Table.Head>;
}
export const THEAD = "text-left text-xs uppercase tracking-wide text-muted-foreground";
