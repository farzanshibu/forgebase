import { useState } from "react";
import { backupSize, createBackup, deleteBackup, listBackups, pruneBackups, verifyBackup } from "../../api";
import type { BackupSnapshot, Retention, VerifyReport } from "../../api";
import { Check } from "../../components/controls";
import { EmptyState } from "../../components/EmptyState";
import { PageHeader } from "../../components/PageHeader";
import { SkeletonTable } from "../../components/Skeleton";
import { Button, ConfirmButton, ErrorView, Panel } from "../../components/ui";
import { formatBytes } from "../../format";
import { useLoad } from "../../hooks";
import { when } from "../functions/util";
import { parseCount } from "./util";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";

function Verified({ r }: { r: VerifyReport | "running" | undefined }) {
  if (r === undefined) return <span className="text-muted-foreground">not verified</span>;
  if (r === "running") return <span className="text-muted-foreground">verifying…</span>;
  return r.ok ? (
    <span className="rounded bg-success/15 px-2 py-0.5 text-xs font-medium text-success" title={`${r.files_checked} files, ${formatBytes(r.bytes_checked)} checked`}>verified</span>
  ) : (
    <span className="rounded bg-destructive/15 px-2 py-0.5 text-xs font-medium text-destructive">{r.problems.length} problem{r.problems.length === 1 ? "" : "s"}</span>
  );
}

function RestoreHelp() {
  return (
    <Panel title="Restore">
      <p className="text-sm text-foreground/80">
        Restore is not available here: it replaces the files the running server has open. Stop the server, then run the CLI against its data directory:
      </p>
      <pre aria-label="Restore command" className="mt-2 overflow-x-auto rounded-md bg-background p-3 font-mono text-xs text-foreground">forgebase restore &lt;snapshot-id | latest | RFC 3339 time&gt; --force</pre>
      <ul className="mt-2 list-disc space-y-1 pl-5 text-xs text-muted-foreground">
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
      <PageHeader title="Backups" description="Snapshots of the data directory you can verify now and restore later." docs="docs/adr/0024-backup-restore.md" />
      <Panel title="Take a snapshot">
        <form onSubmit={create} className="flex flex-wrap items-end gap-3 text-sm">
          <label>Label (optional)<Input size="sm" className="ml-2 w-48" value={label} onChange={(e) => setLabel(e.target.value)} /></label>
          <Check checked={indexes} onChange={setIndexes} label="Include indexes" />
          <Button type="submit" tone="primary" disabled={busy || invalid}>Create snapshot</Button>
        </form>
      </Panel>

      <Panel title="Retention">
        <div className="flex flex-wrap items-end gap-3 text-sm">
          <label>Keep last<Input size="sm" className="ml-2 w-20" inputMode="numeric" value={keep} onChange={(e) => setKeep(e.target.value)} aria-invalid={!keepP.ok} /></label>
          <label>Delete older than (days)<Input size="sm" className="ml-2 w-20" inputMode="numeric" value={days} onChange={(e) => setDays(e.target.value)} aria-invalid={!daysP.ok} /></label>
          <ConfirmButton label="Prune now" onConfirm={prune} disabled={busy || invalid || !hasRetention} />
        </div>
        <p className="mt-2 text-xs text-muted-foreground">
          Applied by Prune now, and after each new snapshot when set. The newest “keep last” snapshots are always kept; with neither value set nothing is deleted.
          {invalid && <span role="alert" className="ml-1 text-destructive">Keep last must be a whole number (0 or more) and days a whole number of at least 1.</span>}
        </p>
      </Panel>

      {notice && <p role="status" className="rounded-md border border-success/40 bg-success/10 p-3 text-sm text-success">{notice}</p>}
      <ErrorView error={error} />

      <Panel title="Snapshots" actions={<Button onClick={list.reload}>Refresh</Button>}>
        <ErrorView error={list.error} />
        {list.loading && !list.error && !list.data && <SkeletonTable rows={2} columns={6} label="Loading snapshots" />}
        {list.data && list.data.length === 0 && (
          <EmptyState compact title="No snapshots yet" why={<>A snapshot is a copy of your data you can restore. Take one above, or run <code>forgebase backup</code>.</>} />
        )}
        {list.data && list.data.length > 0 && (
          <Table aria-label="Snapshots" className="w-full text-left text-sm">
            <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground">
              <Table.Row><Table.Head className="py-1">Snapshot</Table.Head><Table.Head>Created</Table.Head><Table.Head>Size</Table.Head><Table.Head>Files</Table.Head><Table.Head>Verified</Table.Head><Table.Head /></Table.Row>
            </Table.Header>
            <Table.Body>
              {list.data.map((s) => (
                <Table.Row key={s.id} className="border-t border-border">
                  <Table.Cell className="py-1.5 font-mono text-xs">{s.id}{s.manifest.label ? <span className="ml-2 font-sans text-muted-foreground">{s.manifest.label}</span> : null}</Table.Cell>
                  <Table.Cell className="text-muted-foreground">{when(s.manifest.created)}</Table.Cell>
                  <Table.Cell>{formatBytes(backupSize(s))}</Table.Cell>
                  <Table.Cell>{s.manifest.files.length}</Table.Cell>
                  <Table.Cell><Verified r={verified[s.id]} /></Table.Cell>
                  <Table.Cell className="space-x-2 py-1 text-right">
                    <Button disabled={busy} onClick={() => verify(s.id)} aria-label={`Verify ${s.id}`}>Verify</Button>
                    <ConfirmButton label="Delete" disabled={busy} onConfirm={() => remove(s)} />
                  </Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </Panel>

      {open && openReport && openReport !== "running" && (
        <Panel title={`Verification of ${open}`} actions={<Button onClick={() => setOpen(undefined)}>Close</Button>}>
          <p className="text-sm">{openReport.ok ? "All files match the manifest and the database passed its integrity check." : "This snapshot cannot be restored."} {openReport.files_checked} files, {formatBytes(openReport.bytes_checked)} checked.</p>
          {openReport.problems.length > 0 && <ul aria-label="Problems" className="mt-2 list-disc pl-5 text-sm text-destructive">{openReport.problems.map((p) => <li key={p}>{p}</li>)}</ul>}
          {openReport.warnings.length > 0 && <ul aria-label="Warnings" className="mt-2 list-disc pl-5 text-sm text-warning">{openReport.warnings.map((p) => <li key={p}>{p}</li>)}</ul>}
        </Panel>
      )}

      <RestoreHelp />
    </div>
  );
}
