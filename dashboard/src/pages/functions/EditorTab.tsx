import { useState } from "react";
import { getVersionSource, listFunctionVersions } from "../../api";
import type { FunctionInfo, VersionSource } from "../../api";
import { ErrorView, Loading } from "../../components/ui";
import { useLoad } from "../../hooks";
import SourceEditor from "./SourceEditor";

/** Edit the active version's source and deploy the result as a new version. */
export default function EditorTab({ info, onChanged }: { info: FunctionInfo; onChanged: () => void }) {
  const [deployed, setDeployed] = useState<number>();
  const key = `editor:${info.name}:${info.active_version}:${info.latest_version}`;
  const active = useLoad(async (s) => {
    const versions = await listFunctionVersions(info.name, s);
    const v = versions.find((x) => x.active) ?? versions[0];
    if (!v || (v.runtime ?? "wasm") === "wasm") return { version: v, source: undefined as VersionSource | undefined };
    return { version: v, source: await getVersionSource(info.name, v.version, s) };
  }, key);

  if (active.error) return <ErrorView error={active.error} />;
  if (active.loading || !active.data) return <Loading what="Loading source" />;
  const { version, source } = active.data;
  if (!version || !source) {
    return <p className="text-sm text-muted-foreground">The active version is a WebAssembly module, which has no source to edit. Upload a new module on the Versions tab, or create a JavaScript or TypeScript function.</p>;
  }
  return (
    <div className="space-y-3">
      {deployed !== undefined && <p role="status" className="text-sm text-success">Deployed v{deployed} (active)</p>}
      <SourceEditor
        key={key}
        name={info.name}
        language={source.language}
        initialSource={source.source}
        storageKey={info.name}
        capabilities={version.capabilities}
        config={version}
        onDeployed={(d) => { setDeployed(d.version.version); onChanged(); }}
      />
    </div>
  );
}
