import { Textarea } from "panelui-web/textarea";
import { EmptyState } from "../../components/EmptyState";
import { useState } from "react";
import { listAcceptances, listAgreements, publishAgreement, type Agreement } from "../../api";
import { Button } from "../../components/ui";
import { Sel } from "../../components/controls";
import { SkeletonTable } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import { ActionError } from "../access/shared";
import { useRun } from "../access/util";
import { Filter, TableCard } from "../billing/shared";
import { Field, TextField } from "./shared";
import { fmtTime, useRealm } from "./util";
import { Table } from "panelui-web/table";
import { Badge } from "../../components/StatusBadge";
import { Th } from "../../components/TableCard";

/** Legal agreements: every published version, publish a new version, and who accepted what. */
export default function AgreementsPage() {
  const realm = useRealm();
  const l = useLoad((s) => listAgreements(realm, undefined, s), `agreements:${realm}`);
  const [kind, setKind] = useState("");
  const [title, setTitle] = useState("");
  const [body, setBody] = useState("");
  const [url, setUrl] = useState("");
  const [open, setOpen] = useState<Agreement>();
  const { run, error, busy } = useRun(l.reload);
  const [filter, setFilter] = useState("");
  const acc = useLoad((s) => listAcceptances({ realm, kind: filter || undefined, limit: 100 }, s), `accept:${realm}:${filter}:${l.data?.length ?? 0}`);
  const kinds = [...new Set(l.data?.map((a) => a.kind))];
  const latest = (k: string) => Math.max(0, ...(l.data ?? []).filter((a) => a.kind === k).map((a) => a.version));
  return (
    <div className="space-y-6">
      <TableCard title="Agreements" count={l.data?.length} description={<>Terms and policies users of realm <span className="font-mono">{realm}</span> must accept. Every published version is kept.</>}>
        <ActionError error={l.error} />
        {l.loading && !l.error && <SkeletonTable rows={2} columns={4} label="Loading agreements" />}
        {l.data?.length === 0 && <EmptyState compact title="No agreements published yet" why="Publish terms or a privacy policy below and users must accept it at sign-up." />}
        {l.data && l.data.length > 0 && (
          <Table framed aria-label="Agreements">
            <Table.Header><Table.Row><Th>Kind</Th><Th>Version</Th><Th>Title</Th><Th>Published</Th><Th><span className="sr-only">Actions</span></Th></Table.Row></Table.Header>
            <Table.Body>
              {l.data.map((a) => (
                <Table.Row key={a.id} aria-selected={open?.id === a.id || undefined}>
                  <Table.Cell className="font-mono text-xs">{a.kind}</Table.Cell>
                  <Table.Cell className="whitespace-nowrap"><span className="tabular-nums">v{a.version}</span>{a.version === latest(a.kind) && <span className="ml-2"><Badge status="current" /></span>}</Table.Cell>
                  <Table.Cell className="font-medium">{a.title}</Table.Cell>
                  <Table.Cell className="text-xs text-muted-foreground">{fmtTime(a.published_at)} by {a.published_by}</Table.Cell>
                  <Table.Cell className="text-right"><Button aria-label={`View ${a.kind} v${a.version}`} onClick={() => setOpen(open?.id === a.id ? undefined : a)}>{open?.id === a.id ? "Hide" : "View"}</Button></Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
        {open && (
          <article aria-label="Agreement text" className="m-4 rounded-lg border border-border bg-background">
            <header className="flex flex-wrap items-start justify-between gap-2 border-b border-border px-4 py-3">
              <div>
                <h3 className="font-semibold">{open.title} <span className="font-normal text-muted-foreground">(v{open.version})</span></h3>
                {open.url && <a href={open.url} target="_blank" rel="noreferrer" className="text-xs text-primary underline-offset-2 hover:underline">{open.url}</a>}
              </div>
              <Button onClick={() => setOpen(undefined)}>Close</Button>
            </header>
            <pre className="max-h-96 overflow-auto whitespace-pre-wrap px-4 py-3 font-sans text-sm leading-relaxed">{open.body}</pre>
          </article>
        )}
      </TableCard>
      <TableCard title="Publish a new version" description="Publishing never edits an old version: it adds the next one, and users accept it at their next sign-in.">
        <form aria-label="Publish agreement" className="space-y-4 p-4" onSubmit={(e) => { e.preventDefault(); void run(async () => { await publishAgreement({ kind, title, body, ...(url ? { url } : {}) }, realm); setBody(""); setTitle(""); }); }}>
          <div className="grid gap-3 md:grid-cols-3">
            <TextField label="Kind" value={kind} onChange={setKind} placeholder="terms, privacy" mono help={kinds.length ? `Existing: ${kinds.join(", ")}` : undefined} />
            <TextField label="Title" value={title} onChange={setTitle} placeholder="Terms of service" />
            <TextField label="URL (optional)" value={url} onChange={setUrl} placeholder="https://example.com/terms" />
          </div>
          <Field label="Body" help="Plain text. Shown to users before they accept."><Textarea aria-label="Body" minRows={6} className="font-mono text-xs" value={body} onChange={(e) => setBody(e.target.value)} /></Field>
          <ActionError error={error} />
          <div className="flex flex-wrap items-center gap-3">
            <Button type="submit" tone="primary" disabled={busy || !kind || !title || !body}>{busy ? "Publishing…" : "Publish version"}</Button>
            {kind && <p className="text-xs text-muted-foreground">Publishing creates version {latest(kind) + 1} of “{kind}”. Users are asked to accept it at their next sign-in.</p>}
          </div>
        </form>
      </TableCard>
      <TableCard
        title="Acceptances"
        count={acc.data?.length}
        description="Who accepted which version, and when."
        toolbar={<Filter label="Kind">{(id) => <Sel placeholder="All kinds" id={id} aria-label="Acceptance kind" value={filter} onChange={setFilter} options={[{ value: "", label: "All kinds" }, ...kinds.map((k) => ({ value: k, label: k }))]} className="w-40" />}</Filter>}
      >
        <ActionError error={acc.error} />
        {acc.data?.length === 0 && <EmptyState compact title="No acceptances recorded" why="Each time a user accepts an agreement it is listed here." />}
        {acc.data && acc.data.length > 0 && (
          <Table framed aria-label="Acceptances">
            <Table.Header><Table.Row><Th>Principal</Th><Th>Agreement</Th><Th>Accepted</Th><Th>IP</Th></Table.Row></Table.Header>
            <Table.Body>
              {acc.data.map((a) => (
                <Table.Row key={a.id}>
                  <Table.Cell className="font-mono text-xs">{a.principal}</Table.Cell>
                  <Table.Cell>{a.kind} v{a.version}</Table.Cell>
                  <Table.Cell className="text-xs text-muted-foreground">{fmtTime(a.accepted_at)}</Table.Cell>
                  <Table.Cell className="font-mono text-xs">{a.ip ?? "—"}</Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
    </div>
  );
}
