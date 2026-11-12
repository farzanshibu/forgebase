import { useNavigate } from "react-router-dom";
import { getVersionSource, listFunctionVersions } from "../../api";
import type { FunctionInfo, VersionSource } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { SkeletonLines } from "../../components/Skeleton";
import { toaster } from "../../toaster";
import { useLoad } from "../../hooks";
import SourceEditor from "./SourceEditor";
import { when } from "./util";

/** Edit the active version's source and deploy the result as a new version. */
export default function EditorTab({ info, onChanged }: { info: FunctionInfo; onChanged: () => void }) {
  const nav = useNavigate();
  const key = `editor:${info.name}:${info.active_version}:${info.latest_version}`;
  const active = useLoad(async (s) => {
    const versions = await listFunctionVersions(info.name, s);
    const v = versions.find((x) => x.active) ?? versions[0];
    if (!v || (v.runtime ?? "wasm") === "wasm") return { version: v, source: undefined as VersionSource | undefined };
    return { version: v, source: await getVersionSource(info.name, v.version, s) };
  }, key);

  if (active.error) return <ErrorPanel error={active.error} onRetry={active.reload} />;
  if (active.loading || !active.data) return <SkeletonLines lines={8} label="Loading source" />;
  const { version, source } = active.data;
  if (!version || !source) {
    return <EmptyState compact title="No source to edit" why="The active version is a WebAssembly module, which has no source to edit. Upload a new module on the Versions tab, or create a JavaScript or TypeScript function." />;
  }
  return (
    <div className="space-y-3">
      <SourceEditor
        key={key}
        name={info.name}
        language={source.language}
        initialSource={source.source}
        storageKey={info.name}
        capabilities={version.capabilities}
        config={version}
        context={<span>Editing the active version <span className="font-mono text-foreground">v{version.version}</span>, deployed <span className="font-mono">{when(version.created_at)}</span>{version.created_by ? ` by ${version.created_by}` : ""}</span>}
        onDeployed={(d) => {
          toaster.add({ title: `Version v${d.version.version} deployed`, description: "It is the active version. Try it here.", type: "success" });
          onChanged();
          nav(`/functions/f/${encodeURIComponent(info.name)}/test`);
        }}
      />
    </div>
  );
}
