import { useState } from "react";
import { DELIVERY_STATUSES, listDeliveries, listTemplates, previewEmail, putTemplate, resetTemplate, retryDelivery, sendTestEmail, type DeliveryStatus, type EmailTemplate, type Preview } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { ActionError, Empty, Field, TextField } from "../access/shared";
import { useRun } from "../access/util";
import { Note } from "./shared";
import { fmtTime, useRealm, nest } from "./util";

/** Email templates (edit, variable help, preview, test send) and the delivery log with retry. */
export default function EmailPage() {
  const realm = useRealm();
  const l = useLoad((s) => listTemplates(realm, s), `tpl:${realm}`);
  const [key, setKey] = useState<string>();
  const tpl = l.data?.find((t) => t.key === key);
  return (
    <div className="space-y-6">
      <Panel title={`Email templates of ${realm}`}>
        <ErrorView error={l.error} />
        {l.loading && !l.error && <Loading what="Loading templates" />}
        {l.data?.length === 0 && <Empty>No templates.</Empty>}
        <ul className="text-sm">
          {l.data?.map((t) => (
            <li key={t.key} className="flex items-center justify-between border-t border-zinc-800 py-1.5">
              <span><span className="font-mono">{t.key}</span> <span className="text-zinc-400">{t.subject}</span> {t.customized && <span className="ml-1 rounded bg-amber-500/15 px-1.5 text-xs text-amber-300">customised</span>}</span>
              <Button aria-label={`Edit template ${t.key}`} onClick={() => setKey(t.key)}>Edit</Button>
            </li>
          ))}
        </ul>
      </Panel>
      {tpl && <TemplateEditor key={`${tpl.key}:${tpl.updated_at}`} tpl={tpl} realm={realm} onSaved={l.reload} />}
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
  return (
    <Panel title={`Edit template ${tpl.key}`}>
      <form aria-label="Edit template" className="space-y-3" onSubmit={(e) => { e.preventDefault(); void save.run(() => putTemplate(tpl.key, draft(), realm)); }}>
        <TextField label="Subject" value={subject} onChange={setSubject} />
        <Field label="Text body"><textarea aria-label="Text body" rows={6} className={`${inputCls} font-mono`} value={text} onChange={(e) => setText(e.target.value)} /></Field>
        <Field label="HTML body (optional)"><textarea aria-label="HTML body" rows={6} className={`${inputCls} font-mono`} value={html} onChange={(e) => setHtml(e.target.value)} /></Field>
        <div aria-label="Template variables" className="text-xs text-zinc-400">
          Variables you can use: {tpl.variables.length === 0 ? "none" : tpl.variables.map((v) => <code key={v} className="mr-2 rounded bg-zinc-800 px-1">{`{{${v}}}`}</code>)}
        </div>
        <div className="flex flex-wrap gap-2">
          <Button type="submit" tone="primary" disabled={save.busy}>Save template</Button>
          {tpl.customized && <ConfirmButton label="Reset to default" onConfirm={() => void save.run(() => resetTemplate(tpl.key, realm))} />}
        </div>
        <ActionError error={save.error} />
      </form>
      <div className="mt-5 space-y-3 border-t border-zinc-800 pt-4">
        <Field label="Sample values for the variables (name=value per line)"><textarea aria-label="Sample values" rows={4} className={`${inputCls} font-mono`} value={vars} onChange={(e) => setVars(e.target.value)} /></Field>
        <div className="flex flex-wrap items-end gap-3">
          <Button aria-label="Preview" disabled={pv.busy} onClick={() => void pv.run(async () => setPreview(await previewEmail({ draft: draft(), variables: variables() }, realm)))}>Preview</Button>
          <TextField label="Send test to" value={to} onChange={setTo} placeholder="you@example.com" />
          <Button disabled={!to || send.busy} onClick={() => void send.run(async () => { setSent(""); await sendTestEmail({ template: tpl.key, to, variables: variables() }, realm); setSent(`Test email queued for ${to}`); })}>Send test</Button>
        </div>
        <Note>The test uses the saved template, so save your edits first.</Note>
        {sent && <p role="status" className="text-sm text-emerald-300">{sent}</p>}
        <ActionError error={pv.error ?? send.error} />
        {preview && (
          <div aria-label="Email preview" className="rounded-md border border-zinc-800 p-3 text-sm">
            <p><span className="text-zinc-500">Subject:</span> {preview.subject}</p>
            <pre className="mt-2 whitespace-pre-wrap font-sans">{preview.text}</pre>
            {preview.html && <iframe title="HTML preview" sandbox="" srcDoc={preview.html} className="mt-2 h-48 w-full rounded bg-white" />}
            {preview.missing.length > 0 && <p role="alert" className="mt-2 text-xs text-amber-300">Missing variables: {preview.missing.join(", ")}</p>}
          </div>
        )}
      </div>
    </Panel>
  );
}

function DeliveryLog({ realm }: { realm: string }) {
  const [status, setStatus] = useState<DeliveryStatus | "">("");
  const l = useLoad((s) => listDeliveries({ realm, status, limit: 50 }, s), `deliveries:${realm}:${status}`);
  const { run, error } = useRun(l.reload);
  return (
    <Panel title="Delivery log" actions={
      <select aria-label="Delivery status" className={inputCls} value={status} onChange={(e) => setStatus(e.target.value as DeliveryStatus | "")}>
        <option value="">all statuses</option>{DELIVERY_STATUSES.map((s) => <option key={s} value={s}>{s}</option>)}
      </select>}>
      <ErrorView error={l.error} />
      {l.loading && !l.error && <Loading what="Loading deliveries" />}
      {l.data?.length === 0 && <Empty>No deliveries.</Empty>}
      {l.data && l.data.length > 0 && (
        <table className="w-full text-left text-sm">
          <thead className="text-xs uppercase tracking-wide text-zinc-500"><tr><th className="py-2">To</th><th>Subject</th><th>Status</th><th>Tries</th><th>Created</th><th /></tr></thead>
          <tbody>
            {l.data.map((d) => (
              <tr key={d.id} className="border-t border-zinc-800 align-top">
                <td className="py-1.5">{d.to}</td>
                <td>{d.subject}{d.last_error && <span className="block text-xs text-red-300">{d.last_error}</span>}</td>
                <td data-status={d.status}>{d.status}</td>
                <td>{d.attempts}</td>
                <td className="text-xs text-zinc-400">{fmtTime(d.created_at)}</td>
                <td className="text-right">{(d.status === "failed" || d.status === "retrying") && <Button aria-label={`Retry ${d.to}`} onClick={() => void run(() => retryDelivery(d.id))}>Retry</Button>}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <ActionError error={error} />
    </Panel>
  );
}
