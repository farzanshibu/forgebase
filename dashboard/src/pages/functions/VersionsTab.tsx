import { useState } from "react";
import { listFunctionVersions, rollbackFunction } from "../../api";
import type { FunctionInfo } from "../../api";
import { Button, ErrorView, Loading } from "../../components/ui";
import { useLoad } from "../../hooks";
import DeployForm from "./DeployForm";
import { when } from "./util";

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
        <p className="text-sm text-zinc-400">Newest first. Rolling back restores the version's code and its whole configuration.</p>
        <div className="flex gap-2">
          <Button onClick={() => void rollback()}>Roll back to previous</Button>
          <Button tone="primary" onClick={() => setDeploying((d) => !d)}>{deploying ? "Cancel" : "Deploy new version"}</Button>
        </div>
      </div>
      {note && <p role="status" className="text-sm text-emerald-300">{note}</p>}
      {deploying && (
        <DeployForm
          key={active?.version ?? 0}
          fixedName={info.name}
          info={info}
          config={active?.config}
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
        <table className="w-full text-left text-sm">
          <thead className="text-xs uppercase tracking-wide text-zinc-500">
            <tr><th className="py-2">Version</th><th>Size</th><th>Capabilities</th><th>Limits</th><th>Deployed</th><th /></tr>
          </thead>
          <tbody>
            {versions.data.map((v) => (
              <tr key={v.version} className="border-t border-zinc-800 align-top">
                <td className="py-2">v{v.version} {v.active && <span className="ml-1 rounded bg-emerald-500/20 px-1.5 py-0.5 text-xs text-emerald-300">active</span>}</td>
                <td>{v.size} B</td>
                <td className="font-mono text-xs">{v.config.capabilities.join(", ") || "none"}</td>
                <td className="font-mono text-xs">{v.config.limits.timeout_ms} ms · {v.config.limits.max_concurrency} concurrent</td>
                <td className="text-zinc-400">{when(v.created_at)} by {v.created_by}</td>
                <td className="text-right">
                  {!v.active && <Button aria-label={`Activate v${v.version}`} onClick={() => void rollback(v.version)}>Activate</Button>}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
