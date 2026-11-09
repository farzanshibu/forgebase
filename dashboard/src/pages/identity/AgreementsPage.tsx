import { useState } from "react";
import { listAcceptances, listAgreements, publishAgreement, type Agreement } from "../../api";
import { Button, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { ActionError, Empty, Field, TextField } from "../access/shared";
import { useRun } from "../access/util";
import { Note } from "./shared";
import { fmtTime, useRealm } from "./util";

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
          <table className="w-full text-left text-sm">
            <thead className="text-xs uppercase tracking-wide text-zinc-500"><tr><th className="py-2">Kind</th><th>Version</th><th>Title</th><th>Published</th><th /></tr></thead>
            <tbody>
              {l.data.map((a) => (
                <tr key={a.id} className="border-t border-zinc-800">
                  <td className="py-1.5 font-mono">{a.kind}</td>
                  <td>v{a.version}{a.version === latest(a.kind) && <span className="ml-2 rounded bg-emerald-500/15 px-1.5 text-xs text-emerald-300">current</span>}</td>
                  <td>{a.title}</td>
                  <td className="text-xs text-zinc-400">{fmtTime(a.published_at)} by {a.published_by}</td>
                  <td className="text-right"><Button aria-label={`View ${a.kind} v${a.version}`} onClick={() => setOpen(a)}>View</Button></td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        {open && <article aria-label="Agreement text" className="mt-3 rounded-md border border-zinc-800 p-3 text-sm"><h3 className="font-medium">{open.title} (v{open.version})</h3>{open.url && <p className="text-xs text-zinc-500">{open.url}</p>}<pre className="mt-2 whitespace-pre-wrap font-sans">{open.body}</pre></article>}
      </Panel>
      <Panel title="Publish a new version">
        <form aria-label="Publish agreement" className="space-y-3" onSubmit={(e) => { e.preventDefault(); void run(async () => { await publishAgreement({ kind, title, body, ...(url ? { url } : {}) }, realm); setBody(""); setTitle(""); }); }}>
          <div className="flex flex-wrap items-end gap-3">
            <TextField label="Kind" value={kind} onChange={setKind} placeholder="terms, privacy" />
            <TextField label="Title" value={title} onChange={setTitle} />
            <TextField label="URL (optional)" value={url} onChange={setUrl} />
          </div>
          <Field label="Body"><textarea aria-label="Body" rows={6} className={`${inputCls} font-mono`} value={body} onChange={(e) => setBody(e.target.value)} /></Field>
          <Button type="submit" tone="primary" disabled={busy || !kind || !title || !body}>Publish version</Button>
          {kind && <Note>Publishing creates version {latest(kind) + 1} of “{kind}”. Users are asked to accept it at their next sign-in.</Note>}
          <ActionError error={error} />
        </form>
      </Panel>
      <Panel title="Acceptances" actions={
        <select aria-label="Acceptance kind" className={inputCls} value={filter} onChange={(e) => setFilter(e.target.value)}>
          <option value="">all kinds</option>{kinds.map((k) => <option key={k} value={k}>{k}</option>)}
        </select>}>
        <ErrorView error={acc.error} />
        {acc.data?.length === 0 && <Empty>No acceptances recorded.</Empty>}
        {acc.data && acc.data.length > 0 && (
          <table className="w-full text-left text-sm">
            <thead className="text-xs uppercase tracking-wide text-zinc-500"><tr><th className="py-2">Principal</th><th>Agreement</th><th>Accepted</th><th>IP</th></tr></thead>
            <tbody>{acc.data.map((a) => <tr key={a.id} className="border-t border-zinc-800"><td className="py-1.5 font-mono text-xs">{a.principal}</td><td>{a.kind} v{a.version}</td><td className="text-xs text-zinc-400">{fmtTime(a.accepted_at)}</td><td className="font-mono text-xs">{a.ip ?? "-"}</td></tr>)}</tbody>
          </table>
        )}
      </Panel>
    </div>
  );
}
