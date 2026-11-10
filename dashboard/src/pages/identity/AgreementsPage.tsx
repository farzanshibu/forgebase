import { Textarea } from "panelui-web/textarea";
import { useState } from "react";
import { listAcceptances, listAgreements, publishAgreement, type Agreement } from "../../api";
import { Button, ErrorView, Loading, Panel } from "../../components/ui";
import { Sel } from "../../components/controls";
import { useLoad } from "../../hooks";
import { ActionError, Empty, Field, TextField } from "../access/shared";
import { useRun } from "../access/util";
import { Note } from "./shared";
import { fmtTime, useRealm } from "./util";
import { Table } from "panelui-web/table";

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
      <Panel title={`Agreements of ${realm}`}>
        <ErrorView error={l.error} />
        {l.loading && !l.error && <Loading what="Loading agreements" />}
        {l.data?.length === 0 && <Empty>No agreements published yet.</Empty>}
        {l.data && l.data.length > 0 && (
          <Table className="w-full text-left text-sm">
            <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground"><Table.Row><Table.Head className="py-2">Kind</Table.Head><Table.Head>Version</Table.Head><Table.Head>Title</Table.Head><Table.Head>Published</Table.Head><Table.Head /></Table.Row></Table.Header>
            <Table.Body>
              {l.data.map((a) => (
                <Table.Row key={a.id} className="border-t border-border">
                  <Table.Cell className="py-1.5 font-mono">{a.kind}</Table.Cell>
                  <Table.Cell>v{a.version}{a.version === latest(a.kind) && <span className="ml-2 rounded bg-success/15 px-1.5 text-xs text-success">current</span>}</Table.Cell>
                  <Table.Cell>{a.title}</Table.Cell>
                  <Table.Cell className="text-xs text-muted-foreground">{fmtTime(a.published_at)} by {a.published_by}</Table.Cell>
                  <Table.Cell className="text-right"><Button aria-label={`View ${a.kind} v${a.version}`} onClick={() => setOpen(a)}>View</Button></Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
        {open && <article aria-label="Agreement text" className="mt-3 rounded-md border border-border p-3 text-sm"><h3 className="font-medium">{open.title} (v{open.version})</h3>{open.url && <p className="text-xs text-muted-foreground">{open.url}</p>}<pre className="mt-2 whitespace-pre-wrap font-sans">{open.body}</pre></article>}
      </Panel>
      <Panel title="Publish a new version">
        <form aria-label="Publish agreement" className="space-y-3" onSubmit={(e) => { e.preventDefault(); void run(async () => { await publishAgreement({ kind, title, body, ...(url ? { url } : {}) }, realm); setBody(""); setTitle(""); }); }}>
          <div className="flex flex-wrap items-end gap-3">
            <TextField label="Kind" value={kind} onChange={setKind} placeholder="terms, privacy" />
            <TextField label="Title" value={title} onChange={setTitle} />
            <TextField label="URL (optional)" value={url} onChange={setUrl} />
          </div>
          <Field label="Body"><Textarea aria-label="Body" minRows={6} className="font-mono" value={body} onChange={(e) => setBody(e.target.value)} /></Field>
          <Button type="submit" tone="primary" disabled={busy || !kind || !title || !body}>Publish version</Button>
          {kind && <Note>Publishing creates version {latest(kind) + 1} of “{kind}”. Users are asked to accept it at their next sign-in.</Note>}
          <ActionError error={error} />
        </form>
      </Panel>
      <Panel title="Acceptances" actions={
        <Sel aria-label="Acceptance kind" value={filter} onChange={setFilter} options={[{ value: "", label: "all kinds" }, ...kinds.map((k) => ({ value: k, label: k }))]} />}>
        <ErrorView error={acc.error} />
        {acc.data?.length === 0 && <Empty>No acceptances recorded.</Empty>}
        {acc.data && acc.data.length > 0 && (
          <Table className="w-full text-left text-sm">
            <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground"><Table.Row><Table.Head className="py-2">Principal</Table.Head><Table.Head>Agreement</Table.Head><Table.Head>Accepted</Table.Head><Table.Head>IP</Table.Head></Table.Row></Table.Header>
            <Table.Body>{acc.data.map((a) => <Table.Row key={a.id} className="border-t border-border"><Table.Cell className="py-1.5 font-mono text-xs">{a.principal}</Table.Cell><Table.Cell>{a.kind} v{a.version}</Table.Cell><Table.Cell className="text-xs text-muted-foreground">{fmtTime(a.accepted_at)}</Table.Cell><Table.Cell className="font-mono text-xs">{a.ip ?? "-"}</Table.Cell></Table.Row>)}</Table.Body>
          </Table>
        )}
      </Panel>
    </div>
  );
}
