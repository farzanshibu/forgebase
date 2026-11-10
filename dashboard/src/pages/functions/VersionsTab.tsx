import { useState } from "react";
import { listFunctionVersions, rollbackFunction } from "../../api";
import type { FunctionInfo } from "../../api";
import { Button, ErrorView, Loading } from "../../components/ui";
import { useLoad } from "../../hooks";
import DeployForm from "./DeployForm";
import { when } from "./util";
import { Table } from "panelui-web/table";

export default function VersionsTab({ info, onChanged }: { info: FunctionInfo; onChanged: () => void }) {
  const versions = useLoad((s) => listFunctionVersions(info.name, s), `versions:${info.name}:${info.active_version}:${info.latest_version}`);
  const [error, setError] = useState<unknown>();
  const [deploying, setDeploying] = useState(false);
  const [note, setNote] = useState("");

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
            <Table.Row><Table.Head className="py-2">Version</Table.Head><Table.Head>Size</Table.Head><Table.Head>Capabilities</Table.Head><Table.Head>Limits</Table.Head><Table.Head>Deployed</Table.Head><Table.Head /></Table.Row>
          </Table.Header>
          <Table.Body>
            {versions.data.map((v) => (
              <Table.Row key={v.version} className="border-t border-border align-top">
                <Table.Cell className="py-2">v{v.version} {v.active && <span className="ml-1 rounded bg-success/15 px-1.5 py-0.5 text-xs text-success">active</span>}</Table.Cell>
                <Table.Cell title={v.sha256}>{v.size} B</Table.Cell>
                <Table.Cell className="font-mono text-xs">{v.capabilities.join(", ") || "none"}</Table.Cell>
                <Table.Cell className="font-mono text-xs">{v.limits.timeout_ms} ms · {v.limits.max_concurrency} concurrent</Table.Cell>
                <Table.Cell className="text-muted-foreground">{when(v.created_at)} by {v.created_by}</Table.Cell>
                <Table.Cell className="text-right">
                  {!v.active && <Button aria-label={`Activate v${v.version}`} onClick={() => void rollback(v.version)}>Activate</Button>}
                </Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
    </div>
  );
}
