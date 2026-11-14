import { useState } from "react";
import { Input } from "panelui-web/input";
import { Textarea } from "panelui-web/textarea";
import { NOTIFIER_KINDS, SEVERITIES, createNotifier, updateNotifier } from "../../api";
import type { AlertSeverity, Notifier, NotifierInput, NotifierKind } from "../../api";
import { CreateSheet } from "../../components/CreateSheet";
import { FormField } from "../../components/FormField";
import { Sel, Toggle } from "../../components/controls";
import { isDuration, matcherLines, parseList, parseMatcherLines } from "./util";

const KIND_HELP: Record<NotifierKind, string> = {
  webhook: "A signed JSON POST to your URL. The signing secret is shown once, when you create it.",
  slack: "A message to a Slack incoming webhook.",
  email: "Email through the server's mail settings and the alert_notification template.",
  pagerduty: "PagerDuty Events API v2: trigger while firing, resolve when it clears.",
  queue: "One message per notification on a ForgeBase topic, for you to fan out.",
};

/**
 * Create or change a notifier. The Slack URL and the PagerDuty routing key are secrets: the server never returns
 * them, so on an edit those fields start empty and a blank one keeps the stored value.
 */
export default function NotifierSheet({ open, onOpenChange, notifier, onSaved }: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  notifier?: Notifier;
  onSaved: (n: Notifier) => void;
}) {
  const editing = !!notifier;
  const [name, setName] = useState(notifier?.name ?? "");
  const [kind, setKind] = useState<NotifierKind>(notifier?.kind ?? "webhook");
  const [url, setUrl] = useState(notifier && notifier.kind !== "slack" ? (notifier.url ?? "") : "");
  const [slackUrl, setSlackUrl] = useState("");
  const [routingKey, setRoutingKey] = useState("");
  const [to, setTo] = useState(notifier?.to.join(", ") ?? "");
  const [topic, setTopic] = useState(notifier?.topic ?? "");
  const [minSeverity, setMinSeverity] = useState<AlertSeverity>(notifier?.min_severity ?? "info");
  const [repeat, setRepeat] = useState(notifier?.repeat_interval ?? "4h");
  const [groupBy, setGroupBy] = useState(notifier?.group_by.join(", ") ?? "alertname");
  const [sendResolved, setSendResolved] = useState(notifier?.send_resolved ?? true);
  const [enabled, setEnabled] = useState(notifier?.enabled ?? true);
  const [matchers, setMatchers] = useState(matcherLines(notifier?.matchers ?? []));

  const m = parseMatcherLines(matchers);
  const nameError = /^[A-Za-z0-9_.-]{1,64}$/.test(name) ? undefined : "Use letters, digits, _, - and . (1 to 64 characters).";
  const repeatError = isDuration(repeat) ? undefined : "Use a length such as 30m, 4h or 1d (1 minute to 7 days).";
  const missing =
    kind === "webhook" ? !url.trim()
    : kind === "slack" ? !editing && !slackUrl.trim()
    : kind === "email" ? parseList(to).length === 0
    : kind === "pagerduty" ? !editing && !routingKey.trim()
    : !topic.trim();

  async function submit() {
    const body: NotifierInput = {
      name, kind, matchers: m.value, min_severity: minSeverity, group_by: parseList(groupBy), repeat_interval: repeat.trim(), send_resolved: sendResolved, enabled,
    };
    if (kind === "webhook") body.url = url.trim();
    if (kind === "slack" && slackUrl.trim()) body.url = slackUrl.trim();
    if (kind === "pagerduty") {
      if (url.trim()) body.url = url.trim();
      if (routingKey.trim()) body.routing_key = routingKey.trim();
    }
    if (kind === "email") body.to = parseList(to);
    if (kind === "queue") body.topic = topic.trim();
    const saved = editing ? await updateNotifier(notifier.id, body) : await createNotifier(body);
    onSaved(saved);
    return { what: `Notifier ${name}` };
  }

  return (
    <CreateSheet
      open={open}
      onOpenChange={onOpenChange}
      title={editing ? `Edit notifier ${notifier.name}` : "New notifier"}
      description="Where firing and resolved alerts are sent. Secrets are write-only: the dashboard never shows them again."
      submitLabel={editing ? "Save notifier" : "Create notifier"}
      toast={editing ? "saved" : "created"}
      canSubmit={!nameError && !missing && !repeatError && !m.error}
      onSubmit={submit}
    >
      <FormField label="Name" error={name ? nameError : undefined} required>{(p) => <Input {...p} size="sm" className="w-full" value={name} onChange={(e) => setName(e.target.value)} />}</FormField>
      <FormField label="Kind" help={KIND_HELP[kind]}>{(p) => <Sel {...p} size="sm" className="w-full" disabled={editing} value={kind} options={NOTIFIER_KINDS} onChange={setKind} />}</FormField>

      {kind === "webhook" && (
        <FormField label="URL" help="A public https:// address." required>{(p) => <Input {...p} size="sm" type="url" className="w-full" value={url} onChange={(e) => setUrl(e.target.value)} />}</FormField>
      )}
      {kind === "slack" && (
        <FormField label="Incoming webhook URL" help={editing ? `Stored sealed${notifier.url ? ` (${notifier.url})` : ""}. Leave empty to keep it; type a new one to replace it.` : "Stored sealed; the dashboard shows only its host afterwards."} required={!editing}>
          {(p) => <Input {...p} size="sm" type="password" autoComplete="off" spellCheck={false} className="w-full" placeholder={editing ? "Unchanged" : "https://hooks.slack.com/services/…"} value={slackUrl} onChange={(e) => setSlackUrl(e.target.value)} />}
        </FormField>
      )}
      {kind === "email" && (
        <FormField label="Recipients" help="One to 20 addresses, separated by commas." required>{(p) => <Input {...p} size="sm" className="w-full" value={to} onChange={(e) => setTo(e.target.value)} />}</FormField>
      )}
      {kind === "pagerduty" && (
        <>
          <FormField label="Routing key" help={editing ? "Stored sealed. Leave empty to keep it; type a new one to replace it." : "The integration key of a PagerDuty Events API v2 integration. Stored sealed."} required={!editing}>
            {(p) => <Input {...p} size="sm" type="password" autoComplete="off" spellCheck={false} className="w-full" placeholder={editing ? "Unchanged" : ""} value={routingKey} onChange={(e) => setRoutingKey(e.target.value)} />}
          </FormField>
          <FormField label="Events URL" help="Optional. The default is https://events.pagerduty.com/v2/enqueue.">{(p) => <Input {...p} size="sm" type="url" className="w-full" value={url} onChange={(e) => setUrl(e.target.value)} />}</FormField>
        </>
      )}
      {kind === "queue" && (
        <FormField label="Topic" help="A ForgeBase topic. Create it in Queues first." required>{(p) => <Input {...p} size="sm" className="w-full" value={topic} onChange={(e) => setTopic(e.target.value)} />}</FormField>
      )}

      <div className="grid grid-cols-2 gap-3">
        <FormField label="Minimum severity">{(p) => <Sel {...p} size="sm" className="w-full" value={minSeverity} options={SEVERITIES} onChange={setMinSeverity} />}</FormField>
        <FormField label="Repeat every" help="While a group keeps firing." error={repeatError}>{(p) => <Input {...p} size="sm" className="w-full" value={repeat} onChange={(e) => setRepeat(e.target.value)} />}</FormField>
      </div>
      <FormField label="Group by" help="Labels that group alerts into one notification, separated by commas.">{(p) => <Input {...p} size="sm" className="w-full" value={groupBy} onChange={(e) => setGroupBy(e.target.value)} />}</FormField>
      <FormField label="Only alerts matching" help="One label=value per line (also !=, =~, !~). Empty sends every alert." error={m.error}>
        {(p) => <Textarea {...p} size="sm" autoResize={false} spellCheck={false} className="h-16 w-full font-mono text-xs" value={matchers} onChange={(e) => setMatchers(e.target.value)} />}
      </FormField>
      <Toggle checked={sendResolved} onChange={setSendResolved} label="Send when resolved" description="Also notify when a group stops firing." />
      <Toggle checked={enabled} onChange={setEnabled} label="Enabled" />
    </CreateSheet>
  );
}
