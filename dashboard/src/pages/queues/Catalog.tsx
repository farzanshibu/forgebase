import { useState } from "react";
import { Link } from "react-router-dom";
import { Input } from "panelui-web/input";
import { queueEventCatalog } from "../../api";
import type { CatalogEventType, CatalogTopic } from "../../api";
import { Button, CopyButton } from "../../components/ui";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { HelpTip } from "../../components/HelpTip";
import { SkeletonLines } from "../../components/Skeleton";
import { Badge } from "../../components/StatusBadge";
import { useLoad } from "../../hooks";
import Tenant from "../jobs/Tenant";
import { Code, Section } from "../jobs/ops";
import { pretty } from "./util";
import { listenCommand, publishCurl, subscribeCurl } from "./webhook";

const EXAMPLE_FROM: Record<CatalogTopic["example_source"], string> = {
  topic: "The topic's own example.",
  schema: "Derived from the schema.",
  none: "Nothing describes this topic's data yet: add a schema or an example on the topic.",
};

/** A JSON schema or example in a block that opens on demand, so a long catalog stays readable. */
function Schema({ label, value }: { label: string; value: unknown }) {
  if (value === undefined || value === null) return null;
  return (
    <details className="rounded-lg border border-border">
      <summary className="cursor-pointer px-3 py-2 text-sm font-medium">{label}</summary>
      <Code label={label} className="rounded-none border-0 border-t">{pretty(value)}</Code>
    </details>
  );
}

function Copy({ curl, listen }: { curl: { label: string; text: string }; listen: string }) {
  return (
    <div className="flex flex-wrap gap-2">
      <CopyButton text={curl.text} label={curl.label} />
      <CopyButton text={listen} label="Copy listen command" />
    </div>
  );
}

function EventType({ topic, e, origin }: { topic: CatalogTopic; e: CatalogEventType; origin: string }) {
  const name = e.type.includes(".") ? e.type : `record.${e.type}`;
  return (
    <li aria-label={`Event ${name}`} className="space-y-2 rounded-lg border border-border p-3">
      <div className="flex flex-wrap items-center gap-2">
        <code className="font-mono text-sm font-semibold">{name}</code>
        <Badge>{e.op}</Badge>
        <span className="text-sm text-muted-foreground">{e.description}</span>
      </div>
      <p className="text-xs text-muted-foreground">Subscribe with this filter to receive only this event:</p>
      <Code label={`${name} filter`}>{JSON.stringify(e.filter)}</Code>
      <Schema label={`${name} data schema`} value={e.data_schema} />
      <p className="text-xs text-muted-foreground">Example message data:</p>
      <Code label={`${name} example`}>{pretty(e.example)}</Code>
      <Copy curl={{ label: "Copy curl", text: subscribeCurl(origin, topic.topic, e.filter) }} listen={listenCommand(topic.topic, e.filter)} />
    </li>
  );
}

function TopicCard({ t, origin }: { t: CatalogTopic; origin: string }) {
  return (
    <Section
      label={`Topic ${t.topic}`}
      title={<Link className="font-mono text-primary hover:underline" to={`/queues/t/${encodeURIComponent(t.topic)}`}>{t.topic}</Link>}
      description={t.description || <span className="text-muted-foreground">No description yet. Add one on the topic page.</span>}
      actions={<span className="flex items-center gap-2 text-xs text-muted-foreground">{t.source && <Badge tone="info">records of {t.source.collection}</Badge>}<Tenant value={t.tenant} /></span>}
    >
      <div className="space-y-3">
        {t.event_types.length > 0 ? (
          <ul aria-label={`Event types of ${t.topic}`} className="space-y-3">
            {t.event_types.map((e) => <EventType key={e.type} topic={t} e={e} origin={origin} />)}
          </ul>
        ) : (
          <>
            <Schema label={`${t.topic} data schema`} value={t.data_schema} />
            <p className="text-xs text-muted-foreground">Example message data. {EXAMPLE_FROM[t.example_source]}</p>
            {t.example_source !== "none" && <Code label={`${t.topic} example`}>{pretty(t.example)}</Code>}
            <Copy curl={{ label: "Copy curl", text: publishCurl(origin, t.topic, t.example ?? {}) }} listen={listenCommand(t.topic)} />
          </>
        )}
      </div>
    </Section>
  );
}

/** What can be subscribed to: every topic you may read with its schema and example, and each record source's created, updated and deleted events. */
export default function Catalog() {
  const cat = useLoad((s) => queueEventCatalog(s), "queue-catalog");
  const [q, setQ] = useState("");
  const origin = typeof window === "undefined" ? "" : window.location.origin;
  const shown = (cat.data ?? []).filter((t) => !q.trim() || t.topic.toLowerCase().includes(q.trim().toLowerCase()) || (t.description ?? "").toLowerCase().includes(q.trim().toLowerCase()));
  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="inline-flex items-center gap-1 text-sm text-muted-foreground">The kinds of event you can subscribe to, with a schema and an example. <HelpTip term="event_catalog" /></p>
        <span className="flex items-center gap-2">
          <Input size="sm" aria-label="Search the catalog" className="w-64" placeholder="Search topics" value={q} onChange={(e) => setQ(e.target.value)} />
          <Button onClick={cat.reload}>Refresh</Button>
        </span>
      </div>
      <ErrorPanel error={cat.error} onRetry={cat.reload} />
      {cat.loading && !cat.data && !cat.error && <SkeletonLines lines={4} label="Loading the event catalog" />}
      {cat.data && cat.data.length === 0 && <EmptyState title="No topics to list" why="A topic you may read appears here with its description, schema and example. Create one on the Topics tab." docs="docs/adr/0031-webhook-tooling.md" />}
      {cat.data && cat.data.length > 0 && shown.length === 0 && <EmptyState compact title="No topic matches" why="Clear the search to see every topic." />}
      {shown.map((t) => <TopicCard key={t.topic} t={t} origin={origin} />)}
    </div>
  );
}
