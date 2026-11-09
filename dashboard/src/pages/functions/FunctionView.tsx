import { NavLink, Route, Routes, useParams } from "react-router-dom";
import { getFunction } from "../../api";
import { ErrorView, Loading } from "../../components/ui";
import { useLoad } from "../../hooks";
import LogsTab from "./LogsTab";
import SecretsTab from "./SecretsTab";
import VersionsTab from "./VersionsTab";

const tab = ({ isActive }: { isActive: boolean }) => `-mb-px border-b-2 px-3 py-2 text-sm ${isActive ? "border-amber-400 text-white" : "border-transparent text-zinc-400 hover:text-zinc-100"}`;

export default function FunctionView() {
  const { name = "" } = useParams();
  const info = useLoad((s) => getFunction(name, s), `fn:${name}`);
  const base = `/functions/f/${encodeURIComponent(name)}`;
  return (
    <div className="space-y-4">
      <div>
        <h2 className="font-mono text-lg">{name}</h2>
        {info.data && (
          <p aria-label="Function summary" className="text-sm text-zinc-400">
            active v{info.data.active_version} of {info.data.latest_version} · HTTP access {info.data.access} · principal <span className="font-mono">{info.data.principal}</span>
          </p>
        )}
      </div>
      <ErrorView error={info.error} />
      {info.loading && !info.error && <Loading what="Loading function" />}
      {info.data && (
        <>
          <nav aria-label="Function tabs" className="flex gap-1 border-b border-zinc-800">
            <NavLink to={base} end className={tab}>Versions</NavLink>
            <NavLink to={`${base}/logs`} className={tab}>Logs</NavLink>
            <NavLink to={`${base}/secrets`} className={tab}>Secrets</NavLink>
          </nav>
          <Routes>
            <Route index element={<VersionsTab info={info.data} onChanged={info.reload} />} />
            <Route path="logs" element={<LogsTab name={name} />} />
            <Route path="secrets" element={<SecretsTab name={name} />} />
          </Routes>
        </>
      )}
    </div>
  );
}
