/**
 * ConfirmDialog: asks before a destructive action and says what is lost (dashboard-ux.md rule 5, audit A-13).
 * For an irreversible action pass `confirmText` (usually the resource name): the confirm button stays disabled
 * until it is typed exactly.
 *
 * Usage:
 *   <ConfirmDialog
 *     trigger={<Button tone="danger">Delete bucket</Button>}
 *     title="Delete bucket avatars?"
 *     consequence="Every file in it and every earlier version is deleted. Apps that read it get 404."
 *     confirmLabel="Delete bucket"
 *     confirmText="avatars"
 *     onConfirm={() => deleteBucket("avatars", alsoObjects)}
 *   >
 *     <Check checked={alsoObjects} onChange={setAlsoObjects} label="Also delete its files" />   // optional extra controls
 *   </ConfirmDialog>
 *
 *   // Controlled (open from a menu item): open={open} onOpenChange={setOpen}, no trigger.
 *   // ConfirmButton in ui.tsx is the old one-line form and wraps this.
 *
 * `onConfirm` may return a promise: the dialog stays open with the button busy until it settles, and closes on
 * success. A thrown error is shown in the dialog with ErrorPanel.
 */
import { AlertDialog } from "panelui-web/alert-dialog";
import { Input } from "panelui-web/input";
import { useId, useState, type ReactElement, type ReactNode } from "react";
import { ErrorPanel } from "./ErrorPanel";

export interface ConfirmDialogProps {
  /** The question: "Delete bucket avatars?" */
  title: string;
  /** What is lost, in plain words. */
  consequence: ReactNode;
  /** The confirm button's words: "Delete bucket". */
  confirmLabel: string;
  /** When set, the user must type this exactly before the confirm button works. */
  confirmText?: string;
  onConfirm: () => void | Promise<unknown>;
  /** The button that opens it (uncontrolled). */
  trigger?: ReactElement;
  /** The trigger's words, when `trigger` is an empty button element. */
  triggerLabel?: ReactNode;
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  /** "destructive" (default) or "warning" for a reversible but disruptive action. */
  tone?: "destructive" | "warning";
  /** Extra controls between the consequence and the buttons. */
  children?: ReactNode;
}

export function ConfirmDialog({ title, consequence, confirmLabel, confirmText, onConfirm, trigger, triggerLabel, open: openProp, onOpenChange, tone = "destructive", children }: ConfirmDialogProps) {
  const [own, setOwn] = useState(false);
  const open = openProp ?? own;
  const [typed, setTyped] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>();
  const inputId = `confirm${useId().replace(/:/g, "")}`;
  const setOpen = (v: boolean) => {
    if (busy && !v) return;
    if (openProp === undefined) setOwn(v);
    onOpenChange?.(v);
    if (!v) {
      setTyped("");
      setError(undefined);
    }
  };
  const matches = confirmText === undefined || typed === confirmText;
  const run = async () => {
    if (!matches || busy) return;
    setBusy(true);
    setError(undefined);
    try {
      await onConfirm();
      setBusy(false);
      setOpen(false);
    } catch (e) {
      setBusy(false);
      setError(e);
    }
  };
  return (
    <AlertDialog open={open} onOpenChange={setOpen}>
      {trigger && (triggerLabel === undefined ? <AlertDialog.Trigger render={trigger} /> : <AlertDialog.Trigger render={trigger}>{triggerLabel}</AlertDialog.Trigger>)}
      <AlertDialog.Content tone={tone} size="sm">
        <AlertDialog.Title>{title}</AlertDialog.Title>
        <AlertDialog.Description>{consequence}</AlertDialog.Description>
        {(confirmText !== undefined || children || error !== undefined) && (
          <div className="space-y-3 text-start text-sm">
            {children}
            {confirmText !== undefined && (
              <div className="space-y-1">
                <label htmlFor={inputId} className="block">
                  Type <span className="font-mono font-semibold">{confirmText}</span> to confirm
                </label>
                <Input
                  id={inputId}
                  value={typed}
                  autoComplete="off"
                  spellCheck={false}
                  onChange={(e) => setTyped(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      void run();
                    }
                  }}
                />
              </div>
            )}
            <ErrorPanel error={error} />
          </div>
        )}
        <AlertDialog.Footer>
          <AlertDialog.Cancel disabled={busy} />
          <AlertDialog.Action
            disabled={!matches || busy}
            onClick={(e) => {
              // Keep the dialog open while the action runs; `run` closes it on success.
              e.preventDefault();
              void run();
            }}
          >
            {busy ? `${confirmLabel}…` : confirmLabel}
          </AlertDialog.Action>
        </AlertDialog.Footer>
      </AlertDialog.Content>
    </AlertDialog>
  );
}

export default ConfirmDialog;
