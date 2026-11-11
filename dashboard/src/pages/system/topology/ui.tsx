/**
 * Small pieces shared by the Database topology screen (FB-176): state chip, lag sparkline, a dialog shell, and
 * ActionDialog, the typed-name confirmation that also handles `403 mfa_step_up_required` (sign in again with the
 * second factor, then the action retries by itself).
 *
 * Usage:
 *   <ActionDialog title="Remove replica eu-read-1?" consequence="…" confirmLabel="Remove replica" confirmText="eu-read-1"
 *     onConfirm={() => removeNode(id, name)} onClose={() => setOpen(false)} />
 */
import { Dialog } from "panelui-web/dialog";
import { Input } from "panelui-web/input";
import { useId, useRef, useState, type FormEvent, type ReactNode } from "react";
import { isMfaChallenge, isStepUpRequired, login, mfaVerify } from "../../../api";
import type { LagSample } from "../../../api";
import { useSession } from "../../../auth/session";
import { ErrorPanel } from "../../../components/ErrorPanel";
import { FormField } from "../../../components/FormField";
import { Button } from "../../../components/ui";
import { formatLag, stateInfo, type Tone } from "./util";

const TONE_CLASS: Record<Tone, string> = {
  ok: "border-success/40 bg-success/10 text-success",
  warn: "border-warning/40 bg-warning/10 text-warning",
  bad: "border-destructive/40 bg-destructive/10 text-destructive",
  neutral: "border-border text-muted-foreground",
};

/** A coloured chip with a dot and the words: colour is never the only signal. */
export function Chip({ tone, children, title }: { tone: Tone; children: ReactNode; title?: string }) {
  return (
    <span title={title} className={`inline-flex items-center gap-1 whitespace-nowrap rounded-full border px-2 py-0.5 text-xs font-medium ${TONE_CLASS[tone]}`}>
      <span aria-hidden="true" className="size-1.5 rounded-full bg-current" />
      {children}
    </span>
  );
}

/** The node's state as a chip; hovering says what it means. */
export function StateChip({ state }: { state: string }) {
  const i = stateInfo(state);
  return <Chip tone={i.tone} title={i.why}>{i.label}</Chip>;
}

/** One hour of replica lag as a small line chart with a text alternative. */
export function Sparkline({ samples, label }: { samples: LagSample[]; label: string }) {
  if (samples.length < 2) return <span className="text-xs text-muted-foreground" title="History starts when the server starts">no history yet</span>;
  const w = 80, h = 22;
  const max = Math.max(1, ...samples.map((s) => s.lag_ms));
  const pts = samples.map((s, i) => `${((i / (samples.length - 1)) * w).toFixed(1)},${(h - 2 - (s.lag_ms / max) * (h - 4)).toFixed(1)}`).join(" ");
  const last = samples[samples.length - 1]!;
  return (
    <svg role="img" aria-label={`${label}: lag over the last hour, now ${formatLag(last.lag_ms)}, highest ${formatLag(max)}`} width={w} height={h} viewBox={`0 0 ${w} ${h}`} className="text-primary">
      <polyline points={pts} fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinejoin="round" strokeLinecap="round" />
    </svg>
  );
}

/** A PanelUI dialog that is open while mounted. */
export function DialogShell({ title, description, onClose, footer, children, size = "md" }: { title: string; description?: ReactNode; onClose: () => void; footer?: ReactNode; children: ReactNode; size?: "sm" | "md" | "lg" }) {
  return (
    <Dialog open onOpenChange={(open) => { if (!open) onClose(); }}>
      <Dialog.Content size={size}>
        <Dialog.Header title={title} description={description} />
        <Dialog.Body className="space-y-3 text-sm">{children}</Dialog.Body>
        {footer && <Dialog.Footer>{footer}</Dialog.Footer>}
      </Dialog.Content>
    </Dialog>
  );
}

/** Sign in again with the second factor. `onDone` runs once the session carries a fresh factor. */
export function StepUpForm({ onDone }: { onDone: () => void }) {
  const session = useSession();
  const [email, setEmail] = useState(session.admin?.email ?? "");
  const [password, setPassword] = useState("");
  const [code, setCode] = useState("");
  const [challenge, setChallenge] = useState<string>();
  const [error, setError] = useState<unknown>();
  const [busy, setBusy] = useState(false);
  const [noMfa, setNoMfa] = useState(false);

  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(undefined);
    try {
      if (challenge) {
        await session.adoptSession(await mfaVerify(challenge, code.trim()));
        onDone();
        return;
      }
      const r = await login(email.trim(), password);
      if (isMfaChallenge(r)) {
        setChallenge(r.mfa_token);
        setPassword("");
      } else {
        // This account has no second factor: signing in again cannot satisfy the server.
        await session.adoptSession(r);
        setNoMfa(true);
      }
    } catch (err) {
      setError(err);
      setCode("");
    } finally {
      setBusy(false);
    }
  }

  return (
    <form aria-label="Sign in again" onSubmit={(e) => void submit(e)} className="space-y-3 rounded-lg border border-warning/50 bg-warning/10 p-3">
      <div>
        <p className="font-medium">Sign in again with your second factor</p>
        <p className="text-xs text-muted-foreground">This change needs a sign-in with your authenticator code from the last 10 minutes. After it, the change runs by itself.</p>
      </div>
      {noMfa ? (
        <p role="alert" className="text-sm">This account has no second factor yet, so the server still refuses. Turn on two-step verification for your account, then try again.</p>
      ) : challenge ? (
        <FormField label="Authentication code" help="The code from your authenticator app, or a recovery code.">
          {(p) => <Input {...p} className="w-full" value={code} onChange={(e) => setCode(e.target.value)} autoComplete="one-time-code" inputMode="numeric" autoFocus required />}
        </FormField>
      ) : (
        <>
          <FormField label="Email">{(p) => <Input {...p} type="email" className="w-full" value={email} onChange={(e) => setEmail(e.target.value)} autoComplete="username" required />}</FormField>
          <FormField label="Password">{(p) => <Input {...p} type="password" className="w-full" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="current-password" required />}</FormField>
        </>
      )}
      <ErrorPanel error={error} />
      {!noMfa && <Button type="submit" tone="primary" disabled={busy || (challenge ? !code.trim() : !email.trim() || !password)}>{busy ? "Signing in…" : challenge ? "Verify and continue" : "Continue"}</Button>}
    </form>
  );
}

export interface ActionDialogProps {
  title: string;
  /** What happens, in plain words. */
  consequence: ReactNode;
  confirmLabel: string;
  /** The name to type before the button works (dashboard-ux.md rule 5). */
  confirmText?: string;
  tone?: "danger" | "primary";
  /** Extra controls between the words and the confirmation. */
  children?: ReactNode;
  /** False keeps the button off (a choice is missing). */
  canConfirm?: boolean;
  onConfirm: () => void | Promise<unknown>;
  onClose: () => void;
  size?: "sm" | "md" | "lg";
}

/** A confirmation dialog: typed name when asked, busy state, error panel, and the step-up sign-in on `mfa_step_up_required`. */
export function ActionDialog({ title, consequence, confirmLabel, confirmText, tone = "danger", children, canConfirm = true, onConfirm, onClose, size = "md" }: ActionDialogProps) {
  const [typed, setTyped] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>();
  const [stepUp, setStepUp] = useState(false);
  const inputId = `confirm${useId().replace(/:/g, "")}`;
  const running = useRef(false);
  const matches = confirmText === undefined || typed === confirmText;

  const run = async () => {
    if (!matches || !canConfirm || running.current) return;
    running.current = true;
    setBusy(true);
    setError(undefined);
    try {
      await onConfirm();
      onClose();
    } catch (e) {
      if (isStepUpRequired(e)) setStepUp(true);
      else setError(e);
    } finally {
      running.current = false;
      setBusy(false);
    }
  };

  return (
    <DialogShell
      title={title}
      size={size}
      onClose={() => { if (!busy) onClose(); }}
      description={consequence}
      footer={
        <>
          <Button onClick={onClose} disabled={busy}>Cancel</Button>
          {!stepUp && <Button tone={tone} disabled={!matches || !canConfirm || busy} onClick={() => void run()}>{busy ? `${confirmLabel}…` : confirmLabel}</Button>}
        </>
      }
    >
      {stepUp ? (
        <StepUpForm onDone={() => { setStepUp(false); void run(); }} />
      ) : (
        <>
          {children}
          {confirmText !== undefined && (
            <div className="space-y-1">
              <label htmlFor={inputId} className="block">Type <span className="font-mono font-semibold">{confirmText}</span> to confirm</label>
              <Input id={inputId} value={typed} autoComplete="off" spellCheck={false} onChange={(e) => setTyped(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); void run(); } }} />
            </div>
          )}
          <ErrorPanel error={error} />
        </>
      )}
    </DialogShell>
  );
}
