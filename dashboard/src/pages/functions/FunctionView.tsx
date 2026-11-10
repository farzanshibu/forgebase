import { Route, Routes, useParams } from "react-router-dom";
import { RouteTabs } from "../../components/controls";
import { getFunction } from "../../api";
import { ErrorView, Loading } from "../../components/ui";
import { useLoad } from "../../hooks";
import EditorTab from "./EditorTab";
import TestTab from "./TestTab";
import LogsTab from "./LogsTab";
import SecretsTab from "./SecretsTab";
import VersionsTab from "./VersionsTab";

export default function FunctionView() {
  const { name = "" } = useParams();
  const info = useLoad((s) => getFunction(name, s), `fn:${name}`);
  const base = `/functions/f/${encodeURIComponent(name)}`;
  return (
    <div className="space-y-4">
      <div>
        <h2 className="font-mono text-lg">{name}</h2>
        {info.data && (
          <p aria-label="Function summary" className="text-sm text-muted-foreground">
            active v{info.data.active_version} of {info.data.latest_version} · HTTP access {info.data.access} · principal <span className="font-mono">{info.data.principal}</span>
          </p>
        )}
      </div>
      <ErrorView error={info.error} />
      {info.loading && !info.error && <Loading what="Loading function" />}
      {info.data && (
        <>
          <RouteTabs label="Function tabs" tabs={[{ to: base, label: "Versions", end: true }, { to: `${base}/code`, label: "Editor" }, { to: `${base}/test`, label: "Test" }, { to: `${base}/logs`, label: "Logs" }, { to: `${base}/secrets`, label: "Secrets" }]} />
          <Routes>
            <Route index element={<VersionsTab info={info.data} onChanged={info.reload} />} />
            <Route path="code" element={<EditorTab info={info.data} onChanged={info.reload} />} />
            <Route path="test" element={<TestTab info={info.data} />} />
            <Route path="logs" element={<LogsTab name={name} />} />
            <Route path="secrets" element={<SecretsTab name={name} />} />
          </Routes>
        </>
      )}
    </div>
  );
}
