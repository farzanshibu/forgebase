import { Route, Routes, useParams } from "react-router-dom";
import { RouteTabs } from "../../components/controls";
import { getFunction } from "../../api";
import { ErrorPanel } from "../../components/ErrorPanel";
import { PageHeader } from "../../components/PageHeader";
import { SkeletonLines } from "../../components/Skeleton";
import { UseFromCodeButton } from "../../components/UseFromCode";
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
  const d = info.data;
  return (
    <div className="space-y-4">
      <PageHeader
        title={name}
        description={d ? <span aria-label="Function summary">Active v{d.active_version} of {d.latest_version} · HTTP access {d.access} · principal <span className="font-mono">{d.principal}</span></span> : "Function"}
        docs="docs/adr/0017-functions.md"
        secondary={<UseFromCodeButton kind="function" name={name} />}
      />
      <ErrorPanel error={info.error} onRetry={info.reload} />
      {info.loading && !info.error && <SkeletonLines lines={3} label="Loading function" />}
      {d && (
        <>
          <RouteTabs label="Function tabs" tabs={[{ to: base, label: "Versions", end: true }, { to: `${base}/code`, label: "Editor" }, { to: `${base}/test`, label: "Test" }, { to: `${base}/logs`, label: "Logs" }, { to: `${base}/secrets`, label: "Secrets" }]} />
          <Routes>
            <Route index element={<VersionsTab info={d} onChanged={info.reload} />} />
            <Route path="code" element={<EditorTab info={d} onChanged={info.reload} />} />
            <Route path="test" element={<TestTab info={d} />} />
            <Route path="logs" element={<LogsTab name={name} />} />
            <Route path="secrets" element={<SecretsTab name={name} />} />
          </Routes>
        </>
      )}
    </div>
  );
}
