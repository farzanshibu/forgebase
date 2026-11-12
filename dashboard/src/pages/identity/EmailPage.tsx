import { Textarea } from "panelui-web/textarea";
import { EmptyState } from "../../components/EmptyState";
import { useState } from "react";
import { DELIVERY_STATUSES, listDeliveries, listTemplates, previewEmail, putTemplate, resetTemplate, retryDelivery, sendTestEmail, type DeliveryStatus, type EmailTemplate, type Preview } from "../../api";
import { Button, ConfirmButton } from "../../components/ui";
import { Sel } from "../../components/controls";
import { SkeletonLines, SkeletonTable } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import { notify } from "../../toaster";
import { ActionError } from "../access/shared";
import { useRun } from "../access/util";
import { Filter, TableCard } from "../billing/shared";
import { Field, Note, TextField } from "./shared";
import { fmtTime, useRealm, nest } from "./util";
import { Table } from "panelui-web/table";
import { Badge } from "../../components/StatusBadge";
import { Th } from "../../components/TableCard";

/** Email templates (edit, variable help, preview, test send) and the delivery log with retry. */
export default function EmailPage() {
  const realm = useRealm();
  const l = useLoad((s) => listTemplates(realm, s), `tpl:${realm}`);
  const [key, setKey] = useState<string>();
  const tpl = l.data?.find((t) => t.key === key);
  return (
    <div className="space-y-6">
      <div className="grid items-start gap-6 xl:grid-cols-[18rem_minmax(0,1fr)]">
        <TableCard title="Templates" count={l.data?.length} description={<>Messages realm <span className="font-mono">{realm}</span> sends.</>}>
          <ActionError error={l.error} />
          {l.loading && !l.error && <div className="p-4"><SkeletonLines lines={4} label="Loading templates" /></div>}
          {l.data?.length === 0 && <EmptyState compact title="No templates" why="Email templates control the messages the server sends." />}
          {l.data && l.data.length > 0 && (
            <ul aria-label="Email templates" className="divide-y divide-border">
              {l.data.map((t) => (
                <li key={t.key}>
                  <button
                    type="button"
                    aria-label={`Edit template ${t.key}`}
                    aria-current={t.key === key || undefined}
                    onClick={() => setKey(t.key)}
                    className="flex w-full flex-col gap-0.5 px-4 py-2.5 text-left hover:bg-muted/50 focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring aria-[current]:bg-muted"
                  >
                    <span className="flex items-center justify-between gap-2">
                      <span className="truncate font-mono text-xs font-medium">{t.key}</span>
                      {t.customized && <Badge status="customised" />}
                    </span>
                    <span className="truncate text-xs text-muted-foreground">{t.subject}</span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </TableCard>
        {tpl ? (
          <TemplateEditor key={`${tpl.key}:${tpl.updated_at}`} tpl={tpl} realm={realm} onSaved={l.reload} />
        ) : (
          <EmptyState compact title="Pick a template" why="Choose a template on the left to edit its subject and body, preview it with sample values and send yourself a test." />
        )}
      </div>
      <DeliveryLog realm={realm} />
    </div>
  );
}

function TemplateEditor({ tpl, realm, onSaved }: { tpl: EmailTemplate; realm: string; onSaved: () => void }) {
  const [subject, setSubject] = useState(tpl.subject);
  const [text, setText] = useState(tpl.text);
  const [html, setHtml] = useState(tpl.html ?? "");
  const [vars, setVars] = useState(tpl.variables.map((v) => `${v}=`).join("\n"));
  const [to, setTo] = useState("");
  const [preview, setPreview] = useState<Preview>();
  const [sent, setSent] = useState("");
  const save = useRun(onSaved);
  const pv = useRun();
  const send = useRun();
  const variables = () => nest(vars);
  const draft = () => ({ subject, text, html: html || null });
  const dirty = subject !== tpl.subject || text !== tpl.text || html !== (tpl.html ?? "");
  return (
    <TableCard
      title={<span className="font-mono">{tpl.key}</span>}
      description={tpl.customized ? "Customised for this realm." : "Using the built-in default."}
      actions={tpl.customized ? <ConfirmButton label="Reset to default" short="Reset to default" consequence="Your changes to this template are deleted and the built-in text is used again." onConfirm={() => void save.run(() => resetTemplate(tpl.key, realm))} /> : undefined}
    >
      <form aria-label="Edit template" className="space-y-4 p-4" onSubmit={(e) => { e.preventDefault(); void save.run(async () => { await putTemplate(tpl.key, draft(), realm); notify.saved(`Template ${tpl.key}`); }); }}>
        <TextField label="Subject" value={subject} onChange={setSubject} />
        <div aria-label="Template variables" className="flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">
          <span>Variables you can use:</span>
          {tpl.variables.length === 0 ? "none" : tpl.variables.map((v) => <code key={v} className="rounded border border-border bg-muted px-1.5 py-0.5 font-mono text-foreground">{`{{${v}}}`}</code>)}
        </div>
        <div className="grid gap-4 xl:grid-cols-2">
          <Field label="Text body"><Textarea aria-label="Text body" minRows={8} className="font-mono text-xs" value={text} onChange={(e) => setText(e.target.value)} /></Field>
          <Field label="HTML body (optional)" help="Blank sends the text body only."><Textarea aria-label="HTML body" minRows={8} className="font-mono text-xs" value={html} onChange={(e) => setHtml(e.target.value)} /></Field>
        </div>
        <ActionError error={save.error} />
        <div className="flex flex-wrap items-center gap-2">
          <Button type="submit" tone="primary" disabled={save.busy}>{save.busy ? "Saving…" : "Save template"}</Button>
          {dirty && <span className="text-xs text-warning">Unsaved changes</span>}
        </div>
      </form>
      <section aria-labelledby="tpl-try" className="space-y-3 border-t border-border bg-muted/20 p-4">
        <h3 id="tpl-try" className="text-sm font-semibold">Try it</h3>
        <Field label="Sample values for the variables" help="One name=value per line; user.name fills {{user.name}}."><Textarea aria-label="Sample values" minRows={3} className="font-mono text-xs" value={vars} onChange={(e) => setVars(e.target.value)} /></Field>
        <div className="flex flex-wrap items-end gap-3">
          <Button aria-label="Preview" disabled={pv.busy} onClick={() => void pv.run(async () => setPreview(await previewEmail({ draft: draft(), variables: variables() }, realm)))}>Preview</Button>
          <span className="mx-1 h-6 w-px bg-border" aria-hidden="true" />
          <TextField label="Send test to" value={to} onChange={setTo} placeholder="you@example.com" className="w-64" />
          <Button disabled={!to || send.busy} onClick={() => void send.run(async () => { setSent(""); await sendTestEmail({ template: tpl.key, to, variables: variables() }, realm); setSent(`Test email queued for ${to}`); })}>Send test</Button>
        </div>
        <Note>The test uses the saved template, so save your edits first.</Note>
        {sent && <p role="status" className="text-sm text-success">{sent}</p>}
        <ActionError error={pv.error ?? send.error} />
        {preview && (
          <div aria-label="Email preview" className="overflow-hidden rounded-lg border border-border bg-card text-sm">
            <p className="border-b border-border px-3 py-2"><span className="text-muted-foreground">Subject:</span> <span className="font-medium">{preview.subject}</span></p>
            <pre className="whitespace-pre-wrap px-3 py-2 font-sans">{preview.text}</pre>
            {preview.html && <iframe title="HTML preview" sandbox="" srcDoc={preview.html} className="h-56 w-full border-t border-border bg-white" />}
            {preview.missing.length > 0 && <p role="alert" className="border-t border-border px-3 py-2 text-xs font-medium text-warning">Missing variables: {preview.missing.join(", ")}</p>}
          </div>
        )}
      </section>
    </TableCard>
  );
}

function DeliveryLog({ realm }: { realm: string }) {
  const [status, setStatus] = useState<DeliveryStatus | "">("");
  const l = useLoad((s) => listDeliveries({ realm, status, limit: 50 }, s), `deliveries:${realm}:${status}`);
  const { run, error } = useRun(l.reload);
  return (
    <TableCard
      title="Delivery log"
      count={l.data?.length}
      description="The latest 50 emails and what happened to them. Failed ones can be retried."
      actions={<Button onClick={l.reload} disabled={l.loading}>Refresh</Button>}
      toolbar={<Filter label="Status">{(id) => <Sel placeholder="All statuses" id={id} aria-label="Delivery status" value={status} onChange={(v) => setStatus(v as DeliveryStatus | "")} options={[{ value: "", label: "All statuses" }, ...DELIVERY_STATUSES.map((s) => ({ value: s, label: s }))]} className="w-40" />}</Filter>}
    >
      <ActionError error={l.error} />
      {l.loading && !l.error && <SkeletonTable rows={3} columns={5} label="Loading deliveries" />}
      {l.data?.length === 0 && <EmptyState compact title="No deliveries" why="Sent emails and their results appear here." />}
      {l.data && l.data.length > 0 && (
        <Table framed aria-label="Deliveries">
          <Table.Header><Table.Row><Th>To</Th><Th>Subject</Th><Th>Status</Th><Th align="end">Tries</Th><Th>Created</Th><Th><span className="sr-only">Actions</span></Th></Table.Row></Table.Header>
          <Table.Body>
            {l.data.map((d) => (
              <Table.Row key={d.id} className="align-top">
                <Table.Cell className="font-mono text-xs">{d.to}</Table.Cell>
                <Table.Cell>{d.subject}{d.last_error && <span className="block text-xs text-destructive">{d.last_error}</span>}</Table.Cell>
                <Table.Cell data-status={d.status}><Badge status={d.status} /></Table.Cell>
                <Table.Cell align="end">{d.attempts}</Table.Cell>
                <Table.Cell className="whitespace-nowrap text-xs text-muted-foreground">{fmtTime(d.created_at)}</Table.Cell>
                <Table.Cell className="text-right">{(d.status === "failed" || d.status === "retrying") && <Button aria-label={`Retry ${d.to}`} onClick={() => void run(() => retryDelivery(d.id))}>Retry</Button>}</Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
      <ActionError error={error} />
    </TableCard>
  );
}
