import { lazy, Suspense, useState } from "react";
import { getVersionSource, listFunctionVersions, rollbackFunction } from "../../api";
import type { FunctionInfo, VersionInfo, VersionSource } from "../../api";
import { Button, ErrorView, Loading } from "../../components/ui";
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
    <div className="space-y-2" aria-label={`Source of v${version.version}`} role="group">
      <div className="flex items-center gap-2 text-sm">
        <span>v{version.version} · {v.language} · read-only</span>
        {current && (
          <>
            <Button aria-pressed={!diff} onClick={() => setDiff(false)}>Source</Button>
            <Button aria-pressed={diff} onClick={() => setDiff(true)}>Diff against current (v{active?.version})</Button>
          </>
        )}
      </div>
      <Suspense fallback={<Loading what="Loading editor" />}>
        {diff && current
          ? <CodeEditor key="diff" label={`Diff v${version.version} against v${active?.version}`} language={v.language} value={v.source} original={current.source} readOnly types={types} height="24rem" />
          : <CodeEditor key="src" label={`v${version.version} source`} language={v.language} value={v.source} readOnly types={types} height="24rem" />}
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
  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <p className="text-sm text-muted-foreground">Newest first. Rolling back restores the version's code and its whole configuration.</p>
        <div className="flex gap-2">
          <Button onClick={() => void rollback()}>Roll back to previous</Button>
          <Button tone="primary" onClick={() => setDeploying((d) => !d)}>{deploying ? "Cancel" : "Deploy new version"}</Button>
        </div>
      </div>
      {note && <p role="status" className="text-sm text-success">{note}</p>}
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
      {versions.loading && !versions.error && <Loading what="Loading versions" />}
      {versions.data && (
        <Table className="w-full text-left text-sm">
          <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground">
            <Table.Row><Table.Head className="py-2">Version</Table.Head><Table.Head>Runtime</Table.Head><Table.Head>Language</Table.Head><Table.Head>Size</Table.Head><Table.Head>Capabilities</Table.Head><Table.Head>Limits</Table.Head><Table.Head>Deployed</Table.Head><Table.Head /></Table.Row>
          </Table.Header>
          <Table.Body>
            {versions.data.map((v) => (
              <Table.Row key={v.version} className="border-t border-border align-top">
                <Table.Cell className="py-2">v{v.version} {v.active && <span className="ml-1 rounded bg-success/15 px-1.5 py-0.5 text-xs text-success">active</span>}</Table.Cell>
                <Table.Cell>{v.runtime ?? "wasm"}</Table.Cell>
                <Table.Cell>{v.language ?? "-"}</Table.Cell>
                <Table.Cell title={v.sha256}>{v.size} B</Table.Cell>
                <Table.Cell className="font-mono text-xs">{v.capabilities.join(", ") || "none"}</Table.Cell>
                <Table.Cell className="font-mono text-xs">{v.limits.timeout_ms} ms · {v.limits.max_concurrency} concurrent</Table.Cell>
                <Table.Cell className="text-muted-foreground">{when(v.created_at)} by {v.created_by}</Table.Cell>
                <Table.Cell className="space-x-2 text-right">
                  {(v.runtime ?? "wasm") === "js" && <Button aria-label={`View source v${v.version}`} aria-pressed={viewing === v.version} onClick={() => setViewing(viewing === v.version ? undefined : v.version)}>Source</Button>}
                  {!v.active && <Button aria-label={`Activate v${v.version}`} onClick={() => void rollback(v.version)}>Activate</Button>}
                </Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
      {viewing !== undefined && versions.data?.find((v) => v.version === viewing) && (
        <SourceView name={info.name} version={versions.data.find((v) => v.version === viewing)!} active={active} />
      )}
    </div>
  );
}
