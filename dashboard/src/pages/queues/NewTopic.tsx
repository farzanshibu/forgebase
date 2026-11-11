import { useState } from "react";
import { createQueueTopic } from "../../api";
import { CreateSheet } from "../../components/CreateSheet";
import { FormField } from "../../components/FormField";
import { fieldErrors } from "../../errors";
import { validate } from "../../validate";
import { Input } from "panelui-web/input";

/** New topic: name, optional retention in days, optional tenant (sent as X-ForgeBase-Tenant, FB-141). Opens from the header and from ?new=1. */
export default function NewTopic({ open, onOpenChange, onCreated }: { open: boolean; onOpenChange: (open: boolean) => void; onCreated?: () => void }) {
  const [name, setName] = useState("");
  const [days, setDays] = useState("");
  const [tenant, setTenant] = useState("");
  const [touched, setTouched] = useState(false);
  const [saveError, setSaveError] = useState<unknown>();
  const n = name.trim();
  const nameError = (touched ? validate.topicName(n) : undefined) ?? fieldErrors(saveError).name;
  const daysError = days && !(Number(days) > 0) ? "Use a number of days above 0, or leave it empty for the default." : undefined;
  return (
    <CreateSheet
      open={open} onOpenChange={onOpenChange}
      title="New topic" description="A topic is a named stream. Publishers send messages to it; each subscription gets its own copy."
      submitLabel="Create topic" canSubmit={!validate.topicName(n) && !daysError} onError={setSaveError}
      onSubmit={async () => {
        const t = tenant.trim();
        const made = await createQueueTopic({ name: n, ...(days ? { retention_ms: Math.round(Number(days) * 86_400_000) } : {}) }, t || undefined);
        setName(""); setDays(""); setTenant(""); setTouched(false); setSaveError(undefined);
        onCreated?.();
        return { what: `Topic ${made.name ?? n}`, href: `/queues/t/${encodeURIComponent(made.name ?? (t ? `${t}/${n}` : n))}` };
      }}
    >
      <FormField label="Name" required help="Letters, digits, dots, dashes, underscores or colons, starting with a letter or digit." error={nameError}>
        {(p) => <Input {...p} size="sm" className="w-full" value={name} onChange={(e) => { setName(e.target.value); setTouched(true); }} />}
      </FormField>
      <FormField label="Retention (days)" help="How long messages are kept. Empty uses the server default (7 days)." error={daysError}>
        {(p) => <Input {...p} size="sm" className="w-full" inputMode="decimal" value={days} onChange={(e) => setDays(e.target.value)} />}
      </FormField>
      <FormField label="Tenant" help="Optional. Creates the topic for one tenant, stored as <tenant>/<name>. Leave empty for an instance-wide topic, or your own tenant if you have one.">
        {(p) => <Input {...p} size="sm" className="w-full" value={tenant} onChange={(e) => setTenant(e.target.value)} />}
      </FormField>
    </CreateSheet>
  );
}
