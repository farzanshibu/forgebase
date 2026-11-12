import { lazy, Suspense, useState } from "react";
import { getVersionSource, listFunctionVersions, rollbackFunction } from "../../api";
import type { FunctionInfo, VersionInfo, VersionSource } from "../../api";
import { Badge } from "panelui-web/badge";
import { SkeletonTable } from "../../components/Skeleton";
import { Button, ErrorView, Loading } from "../../components/ui";
import { formatBytes } from "../../format";
import { useLoad } from "../../hooks";
import DeployForm from "./DeployForm";
import { when } from "./util";
import { Table } from "panelui-web/table";

const CodeEditor = lazy(() => import("./CodeEditor"));

/** A JS/TS version's source, read-only, with a diff against the current (active) version. */
function SourceView({ name, version, active, types }: { name: string; version: VersionInfo; active?: VersionInfo; types?: string }) {
  const [diff, setDiff] = useState(false);
  const src = useLoad(async (s): Promise<{ v: VersionSource; current?: VersionSource }> => ({
    v: await getVersionSource(name, version.version, s),
    current: active && active.version !== version.version && (active.runtime ?? "wasm") === "js" ? await getVersionSource(name, active.version, s) : undefined,
  }), `source:${name}:${version.version}:${active?.version ?? 0}`);
  if (src.error) return <ErrorView error={src.error} />;
  if (!src.data) return <Loading what="Loading source" />;
  const { v, current } = src.data;
  return (
    <div className="overflow-hidden rounded-xl border border-border bg-card" aria-label={`Source of v${version.version}`} role="group">
      <div className="flex flex-wrap items-center justify-between gap-2 border-b border-border bg-muted/40 px-3 py-2 text-sm">
        <span className="flex items-center gap-2"><span className="font-mono font-medium">v{version.version}</span><span className="text-muted-foreground">{v.language} · read-only</span></span>
        {current && (
          <span className="flex gap-2">
            <Button aria-pressed={!diff} onClick={() => setDiff(false)} className={!diff ? "bg-accent" : ""}>Source</Button>
            <Button aria-pressed={diff} onClick={() => setDiff(true)} className={diff ? "bg-accent" : ""}>Diff against current (v{active?.version})</Button>
          </span>
        )}
      </div>
      <Suspense fallback={<div className="p-4"><Loading what="Loading editor" /></div>}>
        {diff && current
          ? <CodeEditor key="diff" frameless label={`Diff v${version.version} against v${active?.version}`} language={v.language} value={v.source} original={current.source} readOnly types={types} height="24rem" />
          : <CodeEditor key="src" frameless label={`v${version.version} source`} language={v.language} value={v.source} readOnly types={types} height="24rem" />}
      </Suspense>
    </div>
  );
}

export default function VersionsTab({ info, onChanged }: { info: FunctionInfo; onChanged: () => void }) {
  const versions = useLoad((s) => listFunctionVersions(info.name, s), `versions:${info.name}:${info.active_version}:${info.latest_version}`);
  const [error, setError] = useState<unknown>();
  const [deploying, setDeploying] = useState(false);
  const [note, setNote] = useState("");
  const [viewing, setViewing] = useState<number>();

  async function rollback(version?: number) {
    setError(undefined);
    setNote("");
    try {
      const f = await rollbackFunction(info.name, version);
      setNote(`Active version is now v${f.active_version}`);
      onChanged();
      versions.reload();
    } catch (e) {
      setError(e);
    }
  }

  const active = versions.data?.find((v) => v.active);
  const n = versions.data?.length ?? 0;
  return (
    <div className="space-y-4">
      {note && <p role="status" className="rounded-lg border border-success/40 bg-success/5 px-3 py-2 text-sm text-success">{note}</p>}
      {deploying && (
        <DeployForm
          key={active?.version ?? 0}
          fixedName={info.name}
          info={info}
          config={active}
          onDone={(d) => {
            setDeploying(false);
            setNote(`Deployed v${d.version.version}${d.version.active ? " (active)" : ""}`);
            onChanged();
            versions.reload();
          }}
        />
      )}
      <ErrorView error={error ?? versions.error} />
      <section aria-labelledby="fn-versions" className="overflow-hidden rounded-xl border border-border bg-card">
        <header className="flex flex-wrap items-center justify-between gap-3 border-b border-border px-4 py-3">
          <div className="min-w-0">
            <h2 id="fn-versions" className="text-sm font-medium">Versions{n ? <span className="ml-2 text-xs font-normal tabular-nums text-muted-foreground">{n}</span> : null}</h2>
            <p className="text-xs text-muted-foreground">Newest first. Activating or rolling back restores that version's code and its whole configuration.</p>
          </div>
          <div className="flex gap-2">
            <Button onClick={() => void rollback()}>Roll back to previous</Button>
            <Button tone="primary" onClick={() => setDeploying((d) => !d)}>{deploying ? "Cancel" : "Deploy new version"}</Button>
          </div>
        </header>
        {versions.loading && !versions.error && <div className="p-4"><SkeletonTable rows={3} columns={6} label="Loading versions" /></div>}
        {versions.data && (
          <Table framed density="compact" className="w-full text-left text-sm">
            <Table.Header className="text-xs text-muted-foreground">
              <Table.Row><Table.Head>Version</Table.Head><Table.Head>Runtime</Table.Head><Table.Head>Language</Table.Head><Table.Head align="end">Size</Table.Head><Table.Head>Capabilities</Table.Head><Table.Head>Limits</Table.Head><Table.Head>Deployed</Table.Head><Table.Head><span className="sr-only">Actions</span></Table.Head></Table.Row>
            </Table.Header>
            <Table.Body>
              {versions.data.map((v) => (
                <Table.Row key={v.version} className={`border-t border-border align-top hover:bg-muted/40 ${viewing === v.version ? "bg-muted/40" : ""}`}>
                  <Table.Cell className="whitespace-nowrap py-2.5"><span className="font-mono font-medium">v{v.version}</span> {v.active && <Badge variant="success" size="sm" dot className="ml-1">active</Badge>}</Table.Cell>
                  <Table.Cell><span className="rounded bg-muted px-1.5 py-0.5 font-mono text-xs">{v.runtime ?? "wasm"}</span></Table.Cell>
                  <Table.Cell className="text-muted-foreground">{v.language ?? "-"}</Table.Cell>
                  <Table.Cell align="end" className="whitespace-nowrap" title={`SHA-256 ${v.sha256}`}>{formatBytes(v.size)}</Table.Cell>
                  <Table.Cell className="max-w-56">
                    {v.capabilities.length === 0 ? <span className="text-muted-foreground">none</span> : (
                      <span className="flex flex-wrap gap-1">{v.capabilities.map((c) => <span key={c} className="rounded border border-border px-1 font-mono text-[0.6875rem] leading-4">{c}</span>)}</span>
                    )}
                  </Table.Cell>
                  <Table.Cell className="whitespace-nowrap font-mono text-xs text-muted-foreground">{v.limits.timeout_ms} ms · {v.limits.max_concurrency} concurrent</Table.Cell>
                  <Table.Cell className="text-xs text-muted-foreground"><span className="block whitespace-nowrap font-mono">{when(v.created_at)}</span><span className="block max-w-48 truncate" title={v.created_by}>by {v.created_by}</span></Table.Cell>
                  <Table.Cell align="end" className="space-x-2 whitespace-nowrap">
                    {(v.runtime ?? "wasm") === "js" && <Button aria-label={`View source v${v.version}`} aria-pressed={viewing === v.version} onClick={() => setViewing(viewing === v.version ? undefined : v.version)}>{viewing === v.version ? "Hide source" : "Source"}</Button>}
                    {!v.active && <Button aria-label={`Activate v${v.version}`} onClick={() => void rollback(v.version)}>Activate</Button>}
                  </Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </section>
      {viewing !== undefined && versions.data?.find((v) => v.version === viewing) && (
        <SourceView name={info.name} version={versions.data.find((v) => v.version === viewing)!} active={active} />
      )}
    </div>
  );
}
