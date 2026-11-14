import { useEffect, useState } from "react";
import { Input } from "panelui-web/input";
import { Textarea } from "panelui-web/textarea";
import { ApiError, createQueueSubscription, listFunctions } from "../../api";
import type { FunctionInfo, NewSubscription as NewSubscriptionBody } from "../../api";
import { Check, Sel } from "../../components/controls";
import { CreateSheet } from "../../components/CreateSheet";
import { FormField } from "../../components/FormField";
import { explain, fieldErrors } from "../../errors";
import { validate } from "../../validate";
import { parseJson } from "./util";
import { checkWebhookUrl, parsePauseAfter, parseSeconds, parseTtl, stashSecret } from "./webhook";

type Mode = "pull" | "webhook" | "function";
const MODES: { value: Mode; label: string }[] = [
  { value: "pull", label: "Pull: my code reads messages" },
  { value: "webhook", label: "Push to a webhook" },
  { value: "function", label: "Push to a function" },
];
const PUSH_CODES = new Set(["invalid_push_endpoint", "push_endpoint_blocked"]);

/** The server's refusal of a push endpoint in plain words: what it means, then its own message (the rule that failed). */
function endpointProblem(error: unknown): string | undefined {
  if (!(error instanceof ApiError) || !error.code || !PUSH_CODES.has(error.code)) return undefined;
  const w = explain(error);
  return `${w.title}. ${w.message}`;
}

/**
 * New subscription for this topic: pull, or push to a webhook or a function. The owner is the signed-in account: record messages
 * reach the subscription as that account sees them. A webhook's signing secret comes back in the create answer only; it is handed
 * to the subscription's page, which shows it once.
 */
export default function NewSubscription({ topic, tenant: topicTenant, open, onOpenChange, onCreated }: { topic: string; tenant?: string; open: boolean; onOpenChange: (open: boolean) => void; onCreated: () => void }) {
  const [name, setName] = useState("");
  const [mode, setMode] = useState<Mode>("pull");
  const [url, setUrl] = useState("");
  const [fn, setFn] = useState("");
  const [fns, setFns] = useState<{ items?: FunctionInfo[]; error?: unknown }>({});
  const [pauseAfter, setPauseAfter] = useState("");
  const [ttl, setTtl] = useState("");
  const [deadline, setDeadline] = useState("30");
  const [attempts, setAttempts] = useState("");
  const [backoffBase, setBackoffBase] = useState("");
  const [backoffMax, setBackoffMax] = useState("");
  const [filter, setFilter] = useState("");
  const [dlt, setDlt] = useState("");
  const [ordering, setOrdering] = useState(false);
  const [start, setStart] = useState<"latest" | "earliest">("latest");
  const [tenant, setTenant] = useState("");
  const [touched, setTouched] = useState(false);
  const [saveError, setSaveError] = useState<unknown>();

  // The function list is fetched when someone first chooses "Push to a function", not on every visit to the topic.
  useEffect(() => {
    if (!open || mode !== "function" || fns.items || fns.error) return;
    let live = true;
    listFunctions().then((items) => live && setFns({ items })).catch((error: unknown) => live && setFns({ error }));
    return () => { live = false; };
  }, [open, mode, fns.items, fns.error]);

  const push = mode !== "pull";
  const n = name.trim();
  const server = fieldErrors(saveError);
  const endpointFromServer = endpointProblem(saveError);
  const exists = saveError instanceof ApiError && saveError.code === "subscription_exists" ? `${explain(saveError).title}. ${explain(saveError).next}` : undefined;
  const nameError = (touched ? validate.topicName(n) : undefined) ?? exists ?? server.name;
  const urlCheck = mode === "webhook" ? checkWebhookUrl(url) : {};
  const urlError = mode === "webhook" ? (touched || url !== "" ? urlCheck.error : undefined) ?? endpointFromServer : undefined;
  const fnError = mode === "function" && touched && !fn ? "Choose the function to call." : undefined;
  const secs = (v: string) => (v && !(Number(v) > 0) ? "Use a number above 0." : undefined);
  const pauseCheck = parsePauseAfter(pauseAfter);
  const pauseError = push && !pauseCheck.ok ? pauseCheck.error : undefined;
  const ttlCheck = parseTtl(ttl);
  const ttlError = !ttlCheck.ok ? ttlCheck.error : undefined;
  const baseCheck = parseSeconds(backoffBase, "Backoff base");
  const maxCheck = parseSeconds(backoffMax, "Backoff cap");
  const attemptsError = attempts && !(/^\d+$/.test(attempts) && Number(attempts) >= 1 && Number(attempts) <= 100) ? "Use a whole number from 1 to 100." : undefined;
  const filterError = filter.trim() && (() => { const p = parseJson(filter); return !p.ok || typeof p.value !== "object" || p.value === null || Array.isArray(p.value) ? "The filter must be a JSON object, for example {\"type\": {\"eq\": \"order.paid\"}}." : undefined; })() || undefined;
  const canSubmit = !validate.topicName(n) && !secs(deadline) && !ttlError && !pauseError && !attemptsError && !filterError && baseCheck.ok && maxCheck.ok
    && (mode === "pull" || (mode === "webhook" ? !urlCheck.error : !!fn));
  const reset = () => {
    setName(""); setUrl(""); setFn(""); setPauseAfter(""); setTtl(""); setAttempts(""); setBackoffBase(""); setBackoffMax(""); setFilter(""); setDlt("");
    setTouched(false); setSaveError(undefined);
  };

  return (
    <CreateSheet
      open={open} onOpenChange={onOpenChange}
      title="New subscription" description={`A subscription reads topic ${topic} with its own cursor. You become its owner.`}
      submitLabel="Create subscription" canSubmit={canSubmit} onError={(e) => { setSaveError(e); setTouched(true); }}
      onSubmit={async () => {
        const t = tenant.trim() || topicTenant;
        const body: NewSubscriptionBody = {
          name: n, topic, ack_deadline_ms: Math.round(Number(deadline || "30") * 1000), ordering, start,
          ...(attempts ? { max_delivery_attempts: Number(attempts) } : {}), ...(dlt ? { dead_letter_topic: dlt.trim() } : {}),
          ...(baseCheck.ok && baseCheck.ms ? { backoff_base_ms: baseCheck.ms } : {}), ...(maxCheck.ok && maxCheck.ms ? { backoff_max_ms: maxCheck.ms } : {}),
          ...(filter.trim() ? { filter: JSON.parse(filter) as unknown } : {}),
          ...(ttlCheck.ok && ttlCheck.ms ? { ttl_ms: ttlCheck.ms } : {}),
          ...(mode === "webhook" ? { push: { kind: "webhook" as const, endpoint: url.trim() } } : mode === "function" ? { push: { kind: "function" as const, endpoint: fn } } : {}),
          ...(push && pauseCheck.ok && pauseCheck.value ? { pause_after_failures: pauseCheck.value } : {}),
        };
        const made = await createQueueSubscription(body, t || undefined);
        const key = made.name ?? (t ? `${t}/${n}` : n);
        // Shown once, on the page the sheet opens next; never kept here.
        if (made.push_secret) stashSecret(key, made.push_secret);
        reset();
        onCreated();
        return { what: `Subscription ${key}`, href: `/queues/s/${encodeURIComponent(key)}` };
      }}
    >
      <FormField label="Name" required help="Letters, digits, dots, dashes, underscores or colons." error={nameError}>
        {(p) => <Input {...p} size="sm" className="w-full" value={name} onChange={(e) => { setName(e.target.value); setTouched(true); }} />}
      </FormField>
      <FormField label="Delivery" help={mode === "pull" ? "Your code pulls messages (or streams them) and acknowledges each one." : mode === "webhook" ? "ForgeBase POSTs each message to your URL, signed, and retries until it answers 2xx." : "ForgeBase invokes one of your functions with each message, with no URL to expose."}>
        {(p) => <Sel {...p} aria-label="Delivery" className="w-full" value={mode} onChange={(v) => { setMode(v as Mode); setSaveError(undefined); }} options={MODES} />}
      </FormField>
      {mode === "webhook" && (
        <div className="space-y-1">
          <FormField label="Endpoint URL" required help="https:// only, no user name or password, and a public address. ForgeBase signs every call; the signing secret is shown once, right after you create the subscription." error={urlError}>
            {(p) => <Input {...p} size="sm" className="w-full font-mono" type="url" inputMode="url" placeholder="https://example.com/hooks/forgebase" autoComplete="off" spellCheck={false} value={url} onChange={(e) => { setUrl(e.target.value); setSaveError(undefined); }} />}
          </FormField>
          {!urlError && urlCheck.warning && <p role="note" className="text-xs text-warning">{urlCheck.warning}</p>}
        </div>
      )}
      {mode === "function" && (
        <FormField label="Function" required help="One of your deployed functions. ForgeBase invokes it with each message; nothing is signed, so there is no secret." error={fnError ?? (fns.error ? "The function list could not be loaded." : undefined)}>
          {(p) => (
            <Sel {...p} aria-label="Function" className="w-full" value={fn} onChange={setFn}
              placeholder={fns.items ? (fns.items.length ? "Choose a function" : "No functions deployed yet") : fns.error ? "Could not load functions" : "Loading functions…"}
              options={(fns.items ?? []).map((f) => ({ value: f.name, label: f.description ? `${f.name}: ${f.description}` : f.name }))} />
          )}
        </FormField>
      )}
      {push && (
        <FormField label="Pause after failures" term="circuit_breaker" help="Optional. After this many failed attempts in a row, delivery pauses so a dead endpoint is not hammered. Messages wait and none is dead-lettered until you resume. Whole number 1 to 1000; empty: never pause." error={pauseError}>
          {(p) => <Input {...p} size="sm" className="w-full" inputMode="numeric" placeholder="never" value={pauseAfter} onChange={(e) => setPauseAfter(e.target.value)} />}
        </FormField>
      )}
      <FormField label="Expires after" help="Optional, for a temporary subscription: it is deleted this long after creation. 30m, 2h, 7d or 1h 30m; from 1 minute to 31 days. Empty: it lives until you delete it." error={ttlError}>
        {(p) => <Input {...p} size="sm" className="w-full" placeholder="never" autoComplete="off" value={ttl} onChange={(e) => setTtl(e.target.value)} />}
      </FormField>
      <FormField label="Ack deadline (seconds)" term="ack_deadline" error={secs(deadline)}>
        {(p) => <Input {...p} size="sm" className="w-full" inputMode="decimal" value={deadline} onChange={(e) => setDeadline(e.target.value)} />}
      </FormField>
      <FormField label="Max attempts" help="After this many failed deliveries a message goes to the dead-letter topic. 1 to 100. Empty: unlimited." error={attemptsError}>
        {(p) => <Input {...p} size="sm" className="w-full" inputMode="numeric" value={attempts} onChange={(e) => setAttempts(e.target.value)} />}
      </FormField>
      <div className="grid grid-cols-2 gap-3">
        <FormField label="Backoff base (seconds)" help="Wait before the first redelivery. Empty: 1 s." error={baseCheck.ok ? undefined : baseCheck.error}>
          {(p) => <Input {...p} size="sm" className="w-full" inputMode="decimal" value={backoffBase} onChange={(e) => setBackoffBase(e.target.value)} />}
        </FormField>
        <FormField label="Backoff cap (seconds)" help="Longest wait. Empty: 10 min." error={maxCheck.ok ? undefined : maxCheck.error}>
          {(p) => <Input {...p} size="sm" className="w-full" inputMode="decimal" value={backoffMax} onChange={(e) => setBackoffMax(e.target.value)} />}
        </FormField>
      </div>
      <FormField label="Dead-letter topic" term="dead_letter" help="Optional. You need permission to publish to it.">
        {(p) => <Input {...p} size="sm" className="w-full" value={dlt} onChange={(e) => setDlt(e.target.value)} />}
      </FormField>
      <FormField label="Filter" help={'Optional JSON on message attributes, for example {"type": {"eq": "order.paid"}}. It cannot be changed later.'} error={filterError}>
        {(p) => <Textarea {...p} size="sm" autoResize={false} spellCheck={false} className="block h-20 w-full font-mono text-xs" value={filter} onChange={(e) => setFilter(e.target.value)} />}
      </FormField>
      <FormField label="Start" help="Latest: only new messages. Earliest: every message still retained.">
        {(p) => <Sel {...p} aria-label="Start" value={start} onChange={(v) => setStart(v as "latest" | "earliest")} options={[{ value: "latest", label: "latest" }, { value: "earliest", label: "earliest" }]} />}
      </FormField>
      <FormField label="Tenant" help="Optional. Defaults to the topic's tenant.">
        {(p) => <Input {...p} size="sm" className="w-full" value={tenant} onChange={(e) => setTenant(e.target.value)} placeholder={topicTenant ?? ""} />}
      </FormField>
      <Check checked={ordering} onChange={setOrdering} label="Ordered" description="Deliver messages with the same ordering key one after another." />
    </CreateSheet>
  );
}
