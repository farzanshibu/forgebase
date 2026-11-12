/**
 * CreateSheet: the one way to create a thing (dashboard-ux.md rule 5, audit A-11). The PageHeader's primary action
 * opens a side sheet with the form; success closes it, shows a toast and opens the new resource. Detail drawers use
 * the same sheet (`DetailSheet`).
 *
 * Usage:
 *   const [creating, setCreating] = useCreateParam();          // also opened by the palette's "New bucket" (?new=1)
 *   <PageHeader studio="storage" primary={<Button tone="primary" onClick={() => setCreating(true)}>New bucket</Button>} />
 *   <CreateSheet
 *     open={creating} onOpenChange={setCreating}
 *     title="New bucket" description="Buckets hold files your app uploads."
 *     submitLabel="Create bucket"
 *     canSubmit={!validate.bucketName(name)}
 *     onSubmit={async () => { await createBucket({ name }); return { what: `Bucket ${name}`, href: `/storage/b/${name}` }; }}
 *   >
 *     <FormField label="Name" error={…}>{(p) => <Input {...p} value={name} onChange={…} />}</FormField>
 *   </CreateSheet>
 *
 * `onSubmit` throws to keep the sheet open: the error shows at the top (field errors go to FormFields via
 * `fieldErrors(error)` when you pass `onError`). Returning `{ what, href }` raises the "created" toast and navigates.
 * An edit form in the same sheet passes `toast="saved"` ("Schema saved"), or `toast={false}` when it toasts itself.
 */
import { Sheet } from "panelui-web/sheet";
import { useState, type FormEvent, type ReactNode } from "react";
import { useNavigate } from "react-router-dom";
import { notify } from "../toaster";
import { Button } from "./Button";
import { ErrorPanel } from "./ErrorPanel";

export interface CreateResult {
  /** For the toast: "Bucket avatars". */
  what: string;
  /** Where the new resource lives; the sheet navigates there. */
  href?: string;
}

export interface CreateSheetProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description?: ReactNode;
  /** Default "Create". */
  submitLabel?: string;
  /** False disables the submit button (inline validation failed). */
  canSubmit?: boolean;
  onSubmit: () => Promise<CreateResult | void>;
  /** Called with a failure, so the form can map `fieldErrors(error)` onto its fields. */
  onError?: (error: unknown) => void;
  /** The toast for a returned `{ what }`: "created" (default), "saved" for an edit form, or false for none. */
  toast?: "created" | "saved" | false;
  children: ReactNode;
}

export function CreateSheet({ open, onOpenChange, title, description, submitLabel = "Create", canSubmit = true, onSubmit, onError, toast = "created", children }: CreateSheetProps) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>();
  const navigate = useNavigate();
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!canSubmit || busy) return;
    setBusy(true);
    setError(undefined);
    try {
      const r = await onSubmit();
      setBusy(false);
      onOpenChange(false);
      if (r) {
        if (toast === "created") notify.created(r.what);
        else if (toast === "saved") notify.saved(r.what);
        if (r.href) navigate(r.href);
      }
    } catch (err) {
      setBusy(false);
      setError(err);
      onError?.(err);
    }
  };
  const change = (v: boolean) => {
    if (busy) return;
    if (!v) setError(undefined);
    onOpenChange(v);
  };
  return (
    <Sheet side="right" open={open} onOpenChange={change}>
      <Sheet.Content size="auto" className="w-full max-w-lg">
        <form aria-label={title} onSubmit={(e) => void submit(e)} className="flex h-full min-h-0 flex-col" noValidate>
          <Sheet.Header title={title} description={description} />
          <Sheet.Body className="space-y-4">
            <ErrorPanel error={error} fields={onError ? "hide" : "list"} />
            {children}
          </Sheet.Body>
          <Sheet.Footer className="flex justify-end gap-2">
            <Button type="button" onClick={() => change(false)} disabled={busy}>
              Cancel
            </Button>
            <Button type="submit" tone="primary" disabled={!canSubmit || busy}>
              {busy ? `${submitLabel}…` : submitLabel}
            </Button>
          </Sheet.Footer>
        </form>
      </Sheet.Content>
    </Sheet>
  );
}

/** A detail drawer in the same side sheet, for a row's details or an edit form. */
export function DetailSheet({ open, onOpenChange, title, description, children, footer }: { open: boolean; onOpenChange: (open: boolean) => void; title: ReactNode; description?: ReactNode; children: ReactNode; footer?: ReactNode }) {
  return (
    <Sheet side="right" open={open} onOpenChange={onOpenChange}>
      <Sheet.Content size="auto" className="w-full max-w-xl">
        <Sheet.Header title={title} description={description} />
        <Sheet.Body className="space-y-4">{children}</Sheet.Body>
        {footer && <Sheet.Footer className="flex justify-end gap-2">{footer}</Sheet.Footer>}
      </Sheet.Content>
    </Sheet>
  );
}

export default CreateSheet;
