import { useState } from "react";
import { backupSize, createBackup, deleteBackup, listBackups, pruneBackups, verifyBackup } from "../../api";
import type { BackupSnapshot, Retention, VerifyReport } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { formatBytes } from "../../format";
import { useLoad } from "../../hooks";
import { when } from "../functions/util";
import { parseCount } from "./util";

function Verified({ r }: { r: VerifyReport | "running" | undefined }) {
  if (r === undefined) return <span className="text-zinc-500">not verified</span>;
  if (r === "running") return <span className="text-zinc-400">verifying…</span>;
  return r.ok ? (
    <span className="rounded bg-emerald-500/15 px-2 py-0.5 text-xs font-medium text-emerald-300" title={`${r.files_checked} files, ${formatBytes(r.bytes_checked)} checked`}>verified</span>
  ) : (
    <span className="rounded bg-red-500/15 px-2 py-0.5 text-xs font-medium text-red-300">{r.problems.length} problem{r.problems.length === 1 ? "" : "s"}</span>
  );
}

function RestoreHelp() {
  return (
    <Panel title="Restore">
      <p className="text-sm text-zinc-300">
        Restore is not available here: it replaces the files the running server has open. Stop the server, then run the CLI against its data directory:
      </p>
      <pre aria-label="Restore command" className="mt-2 overflow-x-auto rounded-md bg-zinc-950 p-3 font-mono text-xs text-zinc-200">forgebase restore &lt;snapshot-id | latest | RFC 3339 time&gt; --force</pre>
      <ul className="mt-2 list-disc space-y-1 pl-5 text-xs text-zinc-400">
        <li>A time restores the newest snapshot taken at or before it.</li>
        <li>The replaced data is kept in <span className="font-mono">backups/pre-restore-*</span>; <span className="font-mono">restore --recover</span> rolls back an interrupted restore.</li>
        <li>The master key is not in a snapshot unless it says so: pass <span className="font-mono">--master-key FILE</span> or <span className="font-mono">--allow-missing-key</span>.</li>
        <li>Verify a snapshot before you need it. Start the server again afterwards.</li>
      </ul>
    </Panel>
  );
}

/** System Studio, Backups tab: snapshots with size, time and verified state; create, verify, prune and delete. Restore is CLI only (ADR-0024). */
export default function BackupsPage() {
  const list = useLoad(listBackups, "backups");
  const [label, setLabel] = useState("");
  const [indexes, setIndexes] = useState(true);
  const [keep, setKeep] = useState("");
  const [days, setDays] = useState("");
  const [error, setError] = useState<unknown>();
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState(false);
  const [verified, setVerified] = useState<Record<string, VerifyReport | "running">>({});
  const [open, setOpen] = useState<string>();

  const keepP = parseCount(keep, 0);
  const daysP = parseCount(days, 1);
  const retention: Retention = { ...(keepP.ok && keepP.value !== undefined ? { keep_last: keepP.value } : {}), ...(daysP.ok && daysP.value !== undefined ? { max_age_days: daysP.value } : {}) };
  const hasRetention = Object.keys(retention).length > 0;

  async function run(fn: () => Promise<void>) {
    setError(undefined);
    setNotice("");
    setBusy(true);
    try {
      await fn();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }

  const create = (e: React.FormEvent) => {
    e.preventDefault();
    void run(async () => {
      const m = await createBackup({ ...(label.trim() ? { label: label.trim() } : {}), include_indexes: indexes, ...(hasRetention ? { retention } : {}) });
      setNotice(`Created snapshot ${m.id}.`);
      setLabel("");
      list.reload();
    });
  };

  const prune = () =>
    void run(async () => {
      const r = await pruneBackups(retention);
      setNotice(r.deleted.length ? `Pruned ${r.deleted.length} snapshot${r.deleted.length === 1 ? "" : "s"}: ${r.deleted.join(", ")}.` : "Nothing to prune.");
      setVerified((v) => Object.fromEntries(Object.entries(v).filter(([id]) => !r.deleted.includes(id))));
      list.reload();
    });

  const verify = (id: string) =>
    void run(async () => {
      setVerified((v) => ({ ...v, [id]: "running" }));
      try {
        const r = await verifyBackup(id);
        setVerified((v) => ({ ...v, [id]: r }));
        setOpen(id);
      } catch (err) {
        setVerified((v) => Object.fromEntries(Object.entries(v).filter(([k]) => k !== id)));
        throw err;
      }
    });

  const remove = (s: BackupSnapshot) =>
    void run(async () => {
      await deleteBackup(s.id);
      setNotice(`Deleted snapshot ${s.id}.`);
      setVerified((v) => Object.fromEntries(Object.entries(v).filter(([k]) => k !== s.id)));
      list.reload();
    });

  const invalid = !keepP.ok || !daysP.ok;
  const openReport = open ? verified[open] : undefined;

  return (
    <div className="max-w-5xl space-y-4">
      <Panel title="Take a snapshot">
        <form onSubmit={create} className="flex flex-wrap items-end gap-3 text-sm">
          <label>Label (optional)<input className={`${inputCls} ml-2 w-48`} value={label} onChange={(e) => setLabel(e.target.value)} /></label>
          <label className="flex items-center gap-2"><input type="checkbox" checked={indexes} onChange={(e) => setIndexes(e.target.checked)} />Include indexes</label>
          <Button type="submit" tone="primary" disabled={busy || invalid}>Create snapshot</Button>
        </form>
      </Panel>

      <Panel title="Retention">
        <div className="flex flex-wrap items-end gap-3 text-sm">
          <label>Keep last<input className={`${inputCls} ml-2 w-20`} inputMode="numeric" value={keep} onChange={(e) => setKeep(e.target.value)} aria-invalid={!keepP.ok} /></label>
          <label>Delete older than (days)<input className={`${inputCls} ml-2 w-20`} inputMode="numeric" value={days} onChange={(e) => setDays(e.target.value)} aria-invalid={!daysP.ok} /></label>
          <ConfirmButton label="Prune now" onConfirm={prune} disabled={busy || invalid || !hasRetention} />
        </div>
        <p className="mt-2 text-xs text-zinc-500">
          Applied by Prune now, and after each new snapshot when set. The newest “keep last” snapshots are always kept; with neither value set nothing is deleted.
          {invalid && <span role="alert" className="ml-1 text-red-300">Keep last must be a whole number (0 or more) and days a whole number of at least 1.</span>}
        </p>
      </Panel>

      {notice && <p role="status" className="rounded-md border border-emerald-800/60 bg-emerald-500/10 p-3 text-sm text-emerald-300">{notice}</p>}
      <ErrorView error={error} />

      <Panel title="Snapshots" actions={<Button onClick={list.reload}>Refresh</Button>}>
        <ErrorView error={list.error} />
        {list.loading && !list.error && !list.data && <Loading what="Loading snapshots" />}
        {list.data && list.data.length === 0 && <p className="text-sm text-zinc-400">No snapshots yet. Take one above, or run <span className="font-mono">forgebase backup</span>.</p>}
        {list.data && list.data.length > 0 && (
          <table aria-label="Snapshots" className="w-full text-left text-sm">
            <thead className="text-xs uppercase tracking-wide text-zinc-500">
              <tr><th className="py-1">Snapshot</th><th>Created</th><th>Size</th><th>Files</th><th>Verified</th><th /></tr>
            </thead>
            <tbody>
              {list.data.map((s) => (
                <tr key={s.id} className="border-t border-zinc-800">
                  <td className="py-1.5 font-mono text-xs">{s.id}{s.manifest.label ? <span className="ml-2 font-sans text-zinc-400">{s.manifest.label}</span> : null}</td>
                  <td className="text-zinc-400">{when(s.manifest.created)}</td>
                  <td>{formatBytes(backupSize(s))}</td>
                  <td>{s.manifest.files.length}</td>
                  <td><Verified r={verified[s.id]} /></td>
                  <td className="space-x-2 py-1 text-right">
                    <Button disabled={busy} onClick={() => verify(s.id)} aria-label={`Verify ${s.id}`}>Verify</Button>
                    <ConfirmButton label="Delete" disabled={busy} onConfirm={() => remove(s)} />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Panel>

      {open && openReport && openReport !== "running" && (
        <Panel title={`Verification of ${open}`} actions={<Button onClick={() => setOpen(undefined)}>Close</Button>}>
          <p className="text-sm">{openReport.ok ? "All files match the manifest and the database passed its integrity check." : "This snapshot cannot be restored."} {openReport.files_checked} files, {formatBytes(openReport.bytes_checked)} checked.</p>
          {openReport.problems.length > 0 && <ul aria-label="Problems" className="mt-2 list-disc pl-5 text-sm text-red-300">{openReport.problems.map((p) => <li key={p}>{p}</li>)}</ul>}
          {openReport.warnings.length > 0 && <ul aria-label="Warnings" className="mt-2 list-disc pl-5 text-sm text-amber-300">{openReport.warnings.map((p) => <li key={p}>{p}</li>)}</ul>}
        </Panel>
      )}

      <RestoreHelp />
    </div>
  );
}
