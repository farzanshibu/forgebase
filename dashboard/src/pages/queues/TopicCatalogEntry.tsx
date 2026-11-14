import { useState } from "react";
import { Input } from "panelui-web/input";
import { Textarea } from "panelui-web/textarea";
import { Link } from "react-router-dom";
import { patchQueueTopic } from "../../api";
import type { QueueTopic } from "../../api";
import { Button, ErrorView } from "../../components/ui";
import { FormField } from "../../components/FormField";
import { HelpTip } from "../../components/HelpTip";
import { Section } from "../jobs/ops";
import { fieldErrors } from "../../errors";
import { done } from "./webhook";
import { parseJson, pretty } from "./util";

/** The topic's description and example, as the Event catalog shows them (ADR-0031 §6). An example must satisfy the topic's schema; the server checks. */
export default function TopicCatalogEntry({ topic, onSaved }: { topic: QueueTopic; onSaved: () => void }) {
  const initialExample = topic.example === undefined || topic.example === null ? "" : pretty(topic.example);
  const [description, setDescription] = useState(topic.description ?? "");
  const [example, setExample] = useState(initialExample);
  const [error, setError] = useState<unknown>();
  const [busy, setBusy] = useState(false);
  const exampleCheck = example.trim() ? parseJson(example) : undefined;
  const exampleError = exampleCheck && !exampleCheck.ok ? "The example is not valid JSON." : undefined;
  const descError = description.length > 1024 ? "Use at most 1024 characters." : fieldErrors(error).description;
  const dirty = description !== (topic.description ?? "") || example !== initialExample;
  async function save(e: React.FormEvent) {
    e.preventDefault();
    if (exampleError || descError) return;
    setBusy(true);
    setError(undefined);
    try {
      await patchQueueTopic(topic.name, {
        description: description.trim(),
        ...(exampleCheck?.ok ? { example: exampleCheck.value } : topic.example !== undefined && topic.example !== null ? { clear_example: true } : {}),
      });
      done("Catalog entry saved");
      onSaved();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }
  return (
    <Section title={<span className="inline-flex items-center gap-1">Catalog entry <HelpTip term="event_catalog" /></span>} description={<>What this topic's events are, and an example. Shown in the <Link className="text-primary hover:underline" to="/queues/catalog">Event catalog</Link>.</>}>
      <form aria-label="Catalog entry" onSubmit={(e) => void save(e)} className="grid gap-4 text-sm lg:grid-cols-2">
        <FormField label="Description" help="At most 1024 characters. Empty removes it." error={descError} className="lg:col-span-2">
          {(p) => <Input {...p} size="sm" className="w-full" value={description} onChange={(e) => setDescription(e.target.value)} />}
        </FormField>
        <FormField label="Example (JSON)" help={topic.schema ? "Must satisfy the topic's schema. Empty removes it." : "An example of a message's data. Empty removes it."} error={exampleError ?? fieldErrors(error).example} className="lg:col-span-2">
          {(p) => <Textarea {...p} size="sm" autoResize={false} spellCheck={false} className="block h-28 w-full font-mono text-xs" value={example} onChange={(e) => setExample(e.target.value)} />}
        </FormField>
        <div className="flex items-center gap-3 lg:col-span-2">
          <Button type="submit" tone="primary" disabled={busy || !dirty || !!exampleError || !!descError}>{busy ? "Saving…" : "Save catalog entry"}</Button>
          {dirty && <span className="text-xs text-warning">Unsaved changes</span>}
        </div>
        <div className="lg:col-span-2"><ErrorView error={error} /></div>
      </form>
    </Section>
  );
}
