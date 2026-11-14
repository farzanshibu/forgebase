import { useState } from "react";
import { Link } from "react-router-dom";
import { Input } from "panelui-web/input";
import { Textarea } from "panelui-web/textarea";
import { ArrowDown01Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { redriveDeadLetters } from "../../api";
import type { DeadLetter, RedriveResult } from "../../api";
import { Button, ErrorView } from "../../components/ui";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { EmptyState } from "../../components/EmptyState";
import { FormField } from "../../components/FormField";
import { HelpTip } from "../../components/HelpTip";
import { Check } from "../../components/controls";
import { Badge } from "../../components/StatusBadge";
import { when } from "../functions/util";
import { Callout, Code, CopyId, Section, When } from "../jobs/ops";
import { done, redriveSummary } from "./webhook";
import { parseJson, pretty } from "./util";

/**
 * Dead letters of a subscription with redrive (ADR-0031 §3): select some, or redrive all with a `since` time and an attribute filter.
 * Both ask first and report a count. A dead letter is redriven once.
 */
export default function DeadLetters({ sub, items, error, onChanged }: { sub: string; items: DeadLetter[] | undefined; error: unknown; onChanged: () => void }) {
  const [open, setOpen] = useState<number>();
  const [picked, setPicked] = useState<number[]>([]);
  const [since, setSince] = useState("");
  const [filterText, setFilterText] = useState("");
  const [result, setResult] = useState<RedriveResult>();
  const redrivable = (items ?? []).filter((d) => !d.redriven_at);
  const chosen = picked.filter((id) => redrivable.some((d) => d.id === id));
  const sinceOk = since === "" || !Number.isNaN(new Date(since).getTime());
  const filterCheck = filterText.trim() === "" ? undefined : parseJson(filterText);
  const filterError = filterCheck && !filterCheck.ok ? "That is not valid JSON." : filterCheck?.ok && (typeof filterCheck.value !== "object" || filterCheck.value === null || Array.isArray(filterCheck.value)) ? "Use a JSON object, such as {\"kind\": \"rush\"}." : undefined;

  async function run(req: Parameters<typeof redriveDeadLetters>[1]) {
    const r = await redriveDeadLetters(sub, req);
    setResult(r);
    setPicked([]);
    done(`Redriven ${r.redriven.length}`, r.remaining > 0 ? `${r.remaining} more match.` : undefined);
    onChanged();
  }

  const actions = items && items.length > 0 ? (
    <>
      <ConfirmDialog
        trigger={<Button disabled={chosen.length === 0}>{chosen.length ? `Redrive selected (${chosen.length})` : "Redrive selected"}</Button>}
        tone="warning"
        title={`Redrive ${chosen.length} dead letter${chosen.length === 1 ? "" : "s"}?`}
        consequence={<>They are delivered to <span className="font-mono">{sub}</span> again with a fresh attempt counter. Each dead letter is redriven once; if the endpoint still fails they can be dead-lettered again as new records.</>}
        confirmLabel="Redrive selected"
        onConfirm={() => run({ ids: chosen })}
      />
      <ConfirmDialog
        trigger={<Button disabled={redrivable.length === 0}>Redrive all…</Button>}
        tone="warning"
        title={`Redrive all dead letters of ${sub}?`}
        consequence={since || filterText.trim() ? "Every dead letter that matches the time and filter below, and was not redriven before, is delivered again with a fresh attempt counter (at most 1000 per call)." : "Every dead letter that was not redriven before is delivered again with a fresh attempt counter (at most 1000 per call). Narrow it with a time or a filter first if you only want some."}
        confirmLabel="Redrive all"
        onConfirm={async () => {
          if (!sinceOk) throw new Error("The time is not valid.");
          if (filterError) throw new Error(filterError);
          await run({ all: true, ...(since ? { since: new Date(since).toISOString() } : {}), ...(filterCheck?.ok ? { filter: filterCheck.value } : {}) });
        }}
      >
        <FormField label="Dead-lettered since" help="Only records dead-lettered at or after this time. Empty: all.">
          {(p) => <Input {...p} size="sm" type="datetime-local" className="w-full" value={since} onChange={(e) => setSince(e.target.value)} />}
        </FormField>
        <FormField label="Attribute filter (JSON)" help="Only messages whose attributes match, in the subscription filter syntax. Empty: all." error={filterError}>
          {(p) => <Textarea {...p} size="sm" autoResize={false} spellCheck={false} className="block h-20 w-full font-mono text-xs" placeholder={'{"kind": "rush"}'} value={filterText} onChange={(e) => setFilterText(e.target.value)} />}
        </FormField>
      </ConfirmDialog>
    </>
  ) : undefined;

  return (
    <Section title={<span className="inline-flex items-center gap-1">Dead letters <HelpTip term="dead_letter" /> <HelpTip term="redrive" /></span>} count={items?.length} actions={actions} description="Messages that failed too many times. Look at the data and the attempt history to find the cause, fix it, then redrive them.">
      <ErrorView error={error} />
      {result && <Callout tone={result.redriven.length > 0 ? "ok" : "info"} role="status">{redriveSummary(result)}</Callout>}
      {items && items.length === 0 && <EmptyState compact title="No dead letters" why="A message lands here after it fails too many times, so you can look at it instead of losing it." />}
      {items && items.length > 0 && (
        <ul aria-label="Dead letters" className="divide-y divide-border rounded-lg border border-border text-sm">
          {items.map((d) => (
            <li key={d.id} className="px-3 py-2.5">
              <div className="flex flex-wrap items-center gap-x-4 gap-y-1">
                <Check aria-label={`Select dead letter ${d.id}`} checked={chosen.includes(d.id)} disabled={!!d.redriven_at} onChange={(on) => setPicked(on ? [...picked, d.id] : picked.filter((x) => x !== d.id))} />
                <span className="min-w-0 max-w-72"><CopyId id={d.message_id} noun="message" /></span>
                <span className="font-medium text-destructive">{d.reason}</span>
                <span className="tabular-nums text-muted-foreground">{d.attempts} attempt(s)</span>
                {d.redriven_at && <Badge tone="info" title={`Redriven by ${d.redriven_by ?? "unknown"}`}>redriven <When iso={d.redriven_at} /></Badge>}
                {d.dead_letter_topic && <span className="text-muted-foreground">forwarded to <Link className="hover:underline" to={`/queues/t/${encodeURIComponent(d.dead_letter_topic)}`}>{d.dead_letter_topic}</Link></span>}
                <span className="ml-auto flex items-center gap-2">
                  <When iso={d.at} className="text-xs text-muted-foreground" />
                  <button type="button" aria-expanded={open === d.id} className="inline-flex items-center gap-1 rounded-md px-1.5 py-0.5 text-primary hover:bg-muted" aria-label={`${open === d.id ? "Hide" : "Show"} dead letter ${d.id}`} onClick={() => setOpen(open === d.id ? undefined : d.id)}>
                    {open === d.id ? "Hide" : "Show"}
                    <HugeiconsIcon icon={ArrowDown01Icon} size={14} strokeWidth={1.8} aria-hidden="true" className={`transition-transform ${open === d.id ? "rotate-180" : ""}`} />
                  </button>
                </span>
              </div>
              {open === d.id && (
                <div className="mt-3 grid gap-3 lg:grid-cols-2">
                  <Code label={`Dead letter ${d.id} data`}>{pretty(d.data)}</Code>
                  <ol aria-label={`Dead letter ${d.id} history`} className="space-y-1.5 text-xs">
                    {d.history.map((h) => (
                      <li key={h.attempt} className="flex flex-wrap gap-x-2">
                        <span className="font-medium">attempt {h.attempt}</span>
                        <span className="text-muted-foreground">at {when(h.at)}: {h.outcome ?? "?"}{h.error ? ` (${h.error})` : ""}{h.consumer ? ` by ${h.consumer}` : ""}</span>
                      </li>
                    ))}
                  </ol>
                </div>
              )}
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}
