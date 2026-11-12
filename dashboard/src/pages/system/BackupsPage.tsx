import { useState } from "react";
import { backupSize, createBackup, deleteBackup, listBackups, pruneBackups, verifyBackup } from "../../api";
import type { BackupSnapshot, Retention, VerifyReport } from "../../api";
import { Check } from "../../components/controls";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { CreateSheet } from "../../components/CreateSheet";
import { EmptyState } from "../../components/EmptyState";
import { FormField } from "../../components/FormField";
import { PageHeader } from "../../components/PageHeader";
import { SkeletonTable } from "../../components/Skeleton";
import { Button, CopyButton, ErrorView } from "../../components/ui";
import { formatBytes } from "../../format";
import { useLoad } from "../../hooks";
import { CardBody, HEAD_ROW, TableCard } from "../../components/TableCard";
import { CopyId, NUM, Section, StatGrid, When } from "../jobs/ops";
import { useNow } from "../jobs/time";
import { parseCount } from "./util";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";

const RESTORE = "forgebase restore <snapshot-id | latest | RFC 3339 time> --force";

function Verified({ r }: { r: VerifyReport | "running" | undefined }) {
  const chip = "inline-flex items-center gap-1 whitespace-nowrap rounded-full border px-2 py-0.5 text-xs font-medium";
  if (r === undefined) return <span className="text-muted-foreground">not verified</span>;
  if (r === "running") return <span className="text-muted-foreground">verifying…</span>;
  return r.ok ? (
    <span className={`${chip} border-success/40 bg-success/10 text-success`} title={`${r.files_checked} files, ${formatBytes(r.bytes_checked)} checked`}>verified</span>
  ) : (
    <span className={`${chip} border-destructive/40 bg-destructive/10 text-destructive`}>{r.problems.length} problem{r.problems.length === 1 ? "" : "s"}</span>
  );
}

function RestoreHelp() {
  return (
    <Section title="Restore" description="Restore is not available here: it replaces the files the running server has open.">
      <p className="text-sm text-foreground/80">Stop the server, then run the CLI against its data directory:</p>
      <div className="mt-2 flex items-start gap-2">
        <pre aria-label="Restore command" className="min-w-0 flex-1 overflow-x-auto rounded-lg border border-border bg-muted/40 p-3 font-mono text-xs text-foreground">forgebase restore &lt;snapshot-id | latest | RFC 3339 time&gt; --force</pre>
        <CopyButton text={RESTORE} label="Copy" />
      </div>
      <ul className="mt-3 list-disc space-y-1 pl-5 text-xs text-muted-foreground">
        <li>A time restores the newest snapshot taken at or before it.</li>
        <li>The replaced data is kept in <span className="font-mono">backups/pre-restore-*</span>; <span className="font-mono">restore --recover</span> rolls back an interrupted restore.</li>
        <li>The master key is not in a snapshot unless it says so: pass <span className="font-mono">--master-key FILE</span> or <span className="font-mono">--allow-missing-key</span>.</li>
        <li>Verify a snapshot before you need it. Start the server again afterwards.</li>
      </ul>
    </Section>
  );
}

/** System studio, Backups tab: snapshots with size, age and verified state; back up now, verify, prune and delete. Restore is CLI only (ADR-0024). */
export default function BackupsPage() {
  const list = useLoad(listBackups, "backups");
  const [creating, setCreating] = useState(false);
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
  const retentionWords = [retention.keep_last !== undefined ? `keeps the newest ${retention.keep_last}` : "", retention.max_age_days !== undefined ? `deletes snapshots older than ${retention.max_age_days} day${retention.max_age_days === 1 ? "" : "s"}` : ""].filter(Boolean).join(" and ");

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

  const prune = () =>
    run(async () => {
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
    run(async () => {
      await deleteBackup(s.id);
      setNotice(`Deleted snapshot ${s.id}.`);
      setVerified((v) => Object.fromEntries(Object.entries(v).filter(([k]) => k !== s.id)));
      list.reload();
    });

  const invalid = !keepP.ok || !daysP.ok;
  const openReport = open ? verified[open] : undefined;
  const items = list.data ?? [];
  const newest = items.reduce<string | undefined>((a, s) => (!a || s.manifest.created > a ? s.manifest.created : a), undefined);
  const oldest = items.reduce<string | undefined>((a, s) => (!a || s.manifest.created < a ? s.manifest.created : a), undefined);
  const totalSize = items.reduce((n, s) => n + backupSize(s), 0);
  const now = useNow();
  const ageDays = newest ? (now - Date.parse(newest)) / 86_400_000 : undefined;

  return (
    <div className="max-w-5xl space-y-4">
      <PageHeader
        title="Backups"
        description="Snapshots of the data directory you can verify now and restore later."
        docs="docs/adr/0024-backup-restore.md"
        secondary={<Button onClick={list.reload}>Refresh</Button>}
        primary={<Button tone="primary" onClick={() => setCreating(true)} disabled={busy}>Back up now</Button>}
      />

      {notice && <p role="status" className="rounded-lg border border-success/40 bg-success/10 p-3 text-sm text-success">{notice}</p>}
      <ErrorView error={error} />

      {items.length > 0 && (
        <StatGrid
          label="Backup summary"
          columns={4}
          items={[
            { label: "Snapshots", value: items.length.toLocaleString() },
            { label: "Total size", value: formatBytes(totalSize) },
            { label: "Newest", value: <When iso={newest} />, tone: ageDays !== undefined && ageDays > 7 ? "warn" : "ok", hint: ageDays !== undefined && ageDays > 7 ? "Older than a week: back up now" : undefined },
            { label: "Oldest", value: <When iso={oldest} /> },
          ]}
        />
      )}

      <TableCard title="Snapshots" count={list.data?.length}>
        <CardBody className="space-y-3">
          <ErrorView error={list.error} />
          {list.loading && !list.error && !list.data && <SkeletonTable rows={2} columns={6} label="Loading snapshots" />}
          {list.data && list.data.length === 0 && (
            <EmptyState
              title="No snapshots yet"
              why={<>A snapshot is a copy of your data you can restore if something goes wrong. Take one now, or run <code className="font-mono">forgebase backup</code> on the server.</>}
              action={<Button tone="primary" onClick={() => setCreating(true)}>Back up now</Button>}
              docs="docs/adr/0024-backup-restore.md"
            />
          )}
        </CardBody>
        {list.data && list.data.length > 0 && (
            <Table framed density="compact" aria-label="Snapshots">
              <Table.Header className={HEAD_ROW}>
                <Table.Row><Table.Head className="py-1">Snapshot</Table.Head><Table.Head className="text-right">Created</Table.Head><Table.Head className={NUM}>Size</Table.Head><Table.Head className={NUM}>Files</Table.Head><Table.Head>Verified</Table.Head><Table.Head><span className="sr-only">Actions</span></Table.Head></Table.Row>
              </Table.Header>
              <Table.Body>
                {list.data.map((s) => (
                  <Table.Row key={s.id}>
                    <Table.Cell className="max-w-72 py-2">
                      <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-0.5">
                        <CopyId id={s.id} noun="snapshot" />
                        {s.manifest.label ? <span className="rounded-md bg-muted px-1.5 py-0.5 text-xs">{s.manifest.label}</span> : null}
                      </div>
                      <span className="block text-xs text-muted-foreground">
                        v{s.manifest.forgebase_version} · schema {s.manifest.schema_version}{s.manifest.includes_indexes ? " · with indexes" : ""}{s.manifest.master_key?.included ? " · with master key" : ""}
                      </span>
                    </Table.Cell>
                    <Table.Cell className="text-right text-muted-foreground"><When iso={s.manifest.created} /></Table.Cell>
                    <Table.Cell className={NUM}>{formatBytes(backupSize(s))}</Table.Cell>
                    <Table.Cell className={NUM}>{s.manifest.files.length}</Table.Cell>
                    <Table.Cell><Verified r={verified[s.id]} /></Table.Cell>
                    <Table.Cell className="py-1.5 text-right">
                      <span className="inline-flex gap-2">
                        <Button disabled={busy} onClick={() => verify(s.id)} aria-label={`Verify ${s.id}`}>Verify</Button>
                        <ConfirmDialog
                          trigger={<Button tone="danger" disabled={busy} aria-label={`Delete ${s.id}`}>Delete</Button>}
                          title={`Delete snapshot ${s.id}?`}
                          consequence="Its files are removed from the backups directory. You can no longer restore from it."
                          confirmLabel="Delete snapshot"
                          onConfirm={() => remove(s)}
                        />
                      </span>
                    </Table.Cell>
                  </Table.Row>
                ))}
              </Table.Body>
            </Table>
        )}
      </TableCard>

      {open && openReport && openReport !== "running" && (
        <Section title={`Verification of ${open}`} actions={<Button onClick={() => setOpen(undefined)}>Close</Button>}>
          <p className={`text-sm ${openReport.ok ? "text-success" : "text-destructive"}`}>{openReport.ok ? "All files match the manifest and the database passed its integrity check." : "This snapshot cannot be restored."} <span className="text-muted-foreground">{openReport.files_checked} files, {formatBytes(openReport.bytes_checked)} checked.</span></p>
          {openReport.problems.length > 0 && <ul aria-label="Problems" className="mt-2 list-disc pl-5 text-sm text-destructive">{openReport.problems.map((p) => <li key={p}>{p}</li>)}</ul>}
          {openReport.warnings.length > 0 && <ul aria-label="Warnings" className="mt-2 list-disc pl-5 text-sm text-warning">{openReport.warnings.map((p) => <li key={p}>{p}</li>)}</ul>}
        </Section>
      )}

      <div className="grid gap-4 lg:grid-cols-2">
        <Section title="Retention" description="Applied by Prune now, and after each new snapshot when set. The newest “keep last” snapshots are always kept; with neither value set nothing is deleted.">
          <div className="grid gap-3 sm:grid-cols-2">
            <FormField label="Keep last" help="Snapshots to keep, newest first." error={keepP.ok ? undefined : "Use a whole number, 0 or more."}>
              {(p) => <Input {...p} size="sm" className="w-full tabular-nums" inputMode="numeric" placeholder="All" value={keep} onChange={(e) => setKeep(e.target.value)} />}
            </FormField>
            <FormField label="Delete older than (days)" help="Age limit in days." error={daysP.ok ? undefined : "Use a whole number of days, 1 or more."}>
              {(p) => <Input {...p} size="sm" className="w-full tabular-nums" inputMode="numeric" placeholder="Never" value={days} onChange={(e) => setDays(e.target.value)} />}
            </FormField>
          </div>
          <div className="mt-4 flex flex-wrap items-center gap-3">
            <ConfirmDialog
              trigger={<Button tone="danger" disabled={busy || invalid || !hasRetention}>Prune now</Button>}
              title="Prune snapshots now?"
              consequence={<>Snapshots outside the retention are deleted: it {retentionWords || "keeps everything"}. You can no longer restore from them.</>}
              confirmLabel="Prune snapshots"
              onConfirm={prune}
            />
            {hasRetention && !invalid && <span className="text-xs text-muted-foreground">Prune {retentionWords}.</span>}
          </div>
        </Section>
        <RestoreHelp />
      </div>

      <CreateSheet
        open={creating}
        onOpenChange={setCreating}
        title="Back up now"
        description="Take a snapshot of the data directory: the database and stored files, and search indexes if you choose. The server keeps running."
        submitLabel="Create snapshot"
        canSubmit={!invalid && !busy}
        onSubmit={async () => {
          setError(undefined);
          setNotice("");
          const m = await createBackup({ ...(label.trim() ? { label: label.trim() } : {}), include_indexes: indexes, ...(hasRetention ? { retention } : {}) });
          setLabel("");
          list.reload();
          return { what: `Snapshot ${m.id}` };
        }}
      >
        <FormField label="Label" help="Optional. Words to find it by later, such as “before upgrade”.">
          {(p) => <Input {...p} size="sm" className="w-full" value={label} onChange={(e) => setLabel(e.target.value)} />}
        </FormField>
        <Check checked={indexes} onChange={setIndexes} label="Include indexes" description="Search and vector indexes. Larger, but a restore needs no rebuild." />
        <p className="rounded-lg border border-border bg-muted/40 p-3 text-xs text-muted-foreground">
          {invalid ? "Fix the retention values on the page first." : hasRetention ? `After the snapshot, the retention runs: it ${retentionWords}.` : "No retention is set, so no older snapshot is deleted."}
        </p>
      </CreateSheet>
    </div>
  );
}
