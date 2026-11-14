import { useState } from "react";
import { Input } from "panelui-web/input";
import { expirePreviousQueueSecret, patchQueueSubscription, pauseQueueSubscription, resumeQueueSubscription, retryQueueNow, rotateQueueSecret } from "../../api";
import type { PushHealth, QueueSubscription, SubscriptionMetrics } from "../../api";
import { Button, ErrorView } from "../../components/ui";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { FormField } from "../../components/FormField";
import { HelpTip } from "../../components/HelpTip";
import { Sel } from "../../components/controls";
import { SecretOnce } from "../identity/shared";
import { Callout, Facts, Section, StatGrid, When } from "../jobs/ops";
import type { StatItem } from "../jobs/ops";
import { Badge } from "../../components/StatusBadge";
import { GRACE_OPTIONS, done, parsePauseAfter } from "./webhook";

const pct = (r: number) => `${+(r * 100).toFixed(1)}%`;
const ms = (v: number | null | undefined) => (v == null ? "none" : `${v} ms`);

/** The cards of the endpoint health: success rate, latency percentiles, consecutive failures and the last good and bad call. */
export function HealthStats({ h }: { h: PushHealth }) {
  const rate = h.success_rate;
  const items: StatItem[] = [
    { label: "Success rate", value: rate == null ? "no attempts" : pct(rate), tone: rate == null ? undefined : rate >= 0.95 ? "ok" : rate >= 0.5 ? "warn" : "bad", hint: `last hour, ${h.attempts} attempt(s)` },
    { label: "p50 latency", value: ms(h.latency_p50_ms) },
    { label: "p95 latency", value: ms(h.latency_p95_ms) },
    { label: "p99 latency", value: ms(h.latency_p99_ms) },
    { label: "Consecutive failures", value: h.consecutive_failures, tone: h.consecutive_failures > 0 ? "bad" : "ok", hint: h.pause_after_failures ? `auto-pause at ${h.pause_after_failures}` : "auto-pause off" },
    { label: "Last success", value: <When iso={h.last_success_at} /> },
    { label: "Last failure", value: <When iso={h.last_failure_at} /> },
  ];
  return <StatGrid label="Endpoint health" columns={7} items={items} />;
}

/**
 * Operating a push subscription (ADR-0031): endpoint health, pause and resume, the auto-pause threshold, "retry now" for deliveries in backoff,
 * and signing-secret rotation with a grace window. The new secret is drawn once, from this component's state only.
 */
export default function PushTools({ sub, metrics, onChanged }: { sub: QueueSubscription; metrics?: SubscriptionMetrics; onChanged: () => void }) {
  const [secret, setSecret] = useState<{ value: string; until?: string | null }>();
  const [grace, setGrace] = useState(String(86_400_000));
  const [reason, setReason] = useState("");
  const [after, setAfter] = useState<string>();
  const [error, setError] = useState<unknown>();
  const [busy, setBusy] = useState(false);
  const push = sub.push!;
  const health = metrics?.push;
  const paused = !!sub.paused_at;
  const isWebhook = push.kind === "webhook";
  const afterText = after ?? (sub.pause_after_failures ? String(sub.pause_after_failures) : "");
  const afterCheck = parsePauseAfter(afterText);
  const afterError = after !== undefined && !afterCheck.ok ? afterCheck.error : undefined;
  const afterDirty = afterCheck.ok && afterCheck.value !== (sub.pause_after_failures ?? 0);
  const waiting = metrics?.delayed ?? 0;

  async function act(fn: () => Promise<unknown>, announce: () => void) {
    setError(undefined);
    setBusy(true);
    try {
      await fn();
      announce();
      onChanged();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      {secret && (
        <SecretOnce
          label={`New signing secret for ${sub.name}`}
          secret={secret.value}
          onDismiss={() => setSecret(undefined)}
          note={<>Store it now: update your receiver to accept it. {secret.until ? <>The old secret also signs until <When iso={secret.until} />, so deliveries keep verifying while you deploy.</> : "The old secret stopped signing at once."}</>}
        />
      )}
      <Section
        title={<span className="inline-flex items-center gap-1">Endpoint health <HelpTip term="circuit_breaker" /></span>}
        description={<>Calls to <span className="font-mono">{push.endpoint}</span> over the last hour. Test sends are not counted.</>}
        actions={paused ? <Badge tone="warn" dot>Paused</Badge> : <Badge tone="ok" dot>Delivering</Badge>}
      >
        <div className="space-y-4">
          {health ? <HealthStats h={health} /> : <p className="text-sm text-muted-foreground">No health figures yet.</p>}
          {health?.last_error && <Callout tone="bad" title="Last error" role="note">{health.last_error}</Callout>}
          {paused && (
            <Callout tone="warn" role="status" title={sub.paused_reason?.startsWith("auto:") ? "Paused automatically" : "Delivery is paused"}>
              <span data-testid="paused-reason">{sub.paused_reason ?? "No reason was given."}</span>{" "}
              Messages keep accumulating and none is dead-lettered while it is paused. Resuming delivers the backlog in order.
            </Callout>
          )}
          <div className="flex flex-wrap items-center gap-2">
            {paused ? (
              <Button tone="primary" disabled={busy} onClick={() => void act(() => resumeQueueSubscription(sub.name), () => done("Delivery resumed", "The backlog is delivered in order."))}>Resume delivery</Button>
            ) : (
              <ConfirmDialog
                trigger={<Button disabled={busy}>Pause delivery</Button>}
                tone="warning"
                title={`Pause delivery of ${sub.name}?`}
                consequence="Nothing is sent to the endpoint. Messages keep accumulating, and none is dead-lettered while it is paused. Resume to deliver the backlog in order."
                confirmLabel="Pause delivery"
                onConfirm={async () => { await pauseQueueSubscription(sub.name, reason.trim() || undefined); setReason(""); done("Delivery paused"); onChanged(); }}
              >
                <FormField label="Reason (optional)" help="Shown as the paused reason. At most 500 characters.">
                  {(p) => <Input {...p} size="sm" className="w-full" maxLength={500} value={reason} onChange={(e) => setReason(e.target.value)} />}
                </FormField>
              </ConfirmDialog>
            )}
            <Button disabled={busy || waiting === 0} title={waiting === 0 ? "No delivery is waiting for a retry backoff." : undefined}
              onClick={() => void act(() => retryQueueNow(sub.name), () => done("Retry started"))}>
              {waiting > 0 ? `Retry now (${waiting} waiting)` : "Retry now"}
            </Button>
          </div>
        </div>
      </Section>
      <Section title={<span className="inline-flex items-center gap-1">Auto-pause <HelpTip term="circuit_breaker" /></span>} description="Stop hammering a dead endpoint: pause after this many failed attempts in a row.">
        <form aria-label="Auto-pause" className="flex flex-wrap items-end gap-3" onSubmit={(e) => {
          e.preventDefault();
          if (!afterCheck.ok) return;
          const v = afterCheck.value;
          void act(() => patchQueueSubscription(sub.name, { pause_after_failures: v }), () => { setAfter(undefined); done(v ? `Auto-pause at ${v} failures` : "Auto-pause off"); });
        }}>
          <FormField label="Pause after failures" help="Whole number from 1 to 1000. Empty turns it off." error={afterError} className="w-64">
            {(p) => <Input {...p} size="sm" inputMode="numeric" placeholder="off" value={afterText} onChange={(e) => setAfter(e.target.value)} />}
          </FormField>
          <Button type="submit" disabled={busy || !afterCheck.ok || !afterDirty}>Save auto-pause</Button>
        </form>
      </Section>
      {isWebhook ? (
        <Section title={<span className="inline-flex items-center gap-1">Signing secret <HelpTip term="signing_secret" /></span>} description="Every call carries X-Forgebase-Signature, signed with this secret. Rotate it without dropping a delivery.">
          <div className="space-y-3">
            <Facts label="Signing secret" items={[
              ["Current", "set (shown once, when it was made)"],
              ["Previous secret", sub.push_secret_previous_expires_at ? <span className="text-warning">also signs until <When iso={sub.push_secret_previous_expires_at} /></span> : "none"],
            ]} />
            {sub.push_secret_previous_expires_at && (
              <p className="text-xs text-muted-foreground">While both sign, the header reads <code className="font-mono">t=…,v1=&lt;new&gt;,v1=&lt;old&gt;</code>: your receiver must accept the request when <em>any</em> v1 matches.</p>
            )}
            <div className="flex flex-wrap items-center gap-2">
              <ConfirmDialog
                trigger={<Button>Rotate secret</Button>}
                tone="warning"
                title={`Rotate the signing secret of ${sub.name}?`}
                consequence={<>A new secret is made and shown <strong>once</strong>. Choose how long the old one keeps signing too <HelpTip term="grace_window" />, so you can deploy the new one first.</>}
                confirmLabel="Rotate secret"
                onConfirm={async () => {
                  const r = await rotateQueueSecret(sub.name, Number(grace));
                  setSecret({ value: r.push_secret, until: r.previous_expires_at });
                  onChanged();
                }}
              >
                <FormField label="Grace window" help="While it lasts, deliveries carry both signatures.">
                  {(p) => <Sel {...p} aria-label="Grace window" className="w-full" value={grace} onChange={setGrace} options={GRACE_OPTIONS} />}
                </FormField>
              </ConfirmDialog>
              {sub.push_secret_previous_expires_at && (
                <ConfirmDialog
                  trigger={<Button tone="danger">Expire previous now</Button>}
                  tone="warning"
                  title="Stop signing with the previous secret?"
                  consequence="Only the new secret signs from now on. A receiver that still has the old secret rejects every delivery."
                  confirmLabel="Expire previous now"
                  onConfirm={async () => { await expirePreviousQueueSecret(sub.name); done("Previous secret expired"); onChanged(); }}
                />
              )}
            </div>
          </div>
        </Section>
      ) : (
        <p role="note" className="rounded-lg border border-border bg-muted/40 p-3 text-sm text-muted-foreground">This subscription pushes to the function <span className="font-mono">{push.endpoint}</span>, which is invoked by the server: nothing is signed, so there is no secret to rotate.</p>
      )}
      <ErrorView error={error} />
    </>
  );
}
