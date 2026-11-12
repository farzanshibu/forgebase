/**
 * BackendInfo: which database engine this server runs on, in one compact line under the Database header.
 * It is the slot FB-155 (Database Studio for PostgreSQL) fills: the element has `data-slot="database-backend"`,
 * and `children` render after the summary, so FB-155 can add its topology and extension controls here.
 *
 * Feature states (FB-161): `active` runs on the PostgreSQL extension, `portable` on ForgeBase's own code, `off` is
 * switched off in Features and settings.
 */
import type { ReactNode } from "react";
import { Database01Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { Link } from "react-router-dom";
import { useLoad } from "../../hooks";
import { BACKEND_NAME, TOPOLOGY_PATH, getDatabaseInfo, type DatabaseFeature, type DatabaseInfo } from "./info";

const STATE_TEXT: Record<DatabaseFeature["state"], string> = { active: "on the extension", portable: "portable", off: "off" };

/** `info` is the studio's already loaded answer; without it the line loads its own. */
export default function BackendInfo({ children, info }: { children?: ReactNode; info?: DatabaseInfo }) {
  const own = useLoad(getDatabaseInfo, "database-info");
  const data = info ?? own.data;
  // A failed or absent answer hides the line: the studio works without it.
  if (!data) return <div data-slot="database-backend">{children}</div>;
  const name = BACKEND_NAME[data.backend] ?? data.backend;
  const features = Object.entries(data.features ?? {});
  const bits = [
    data.server_version ? `${name} ${data.server_version}` : name,
    data.mode ? (data.mode === "managed" ? "bundled engine" : "external server") : undefined,
    typeof data.connections === "number" ? `${data.connections} connections` : undefined,
    data.engine?.version ? `engine ${data.engine.version} (${data.engine.state})` : undefined,
  ].filter(Boolean);
  return (
    <div data-slot="database-backend" aria-label="Database backend" className="mt-3 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
      <span className="inline-flex items-center gap-1.5 rounded-full border border-border bg-card px-2.5 py-0.5 font-medium text-foreground">
        <HugeiconsIcon icon={Database01Icon} size={13} strokeWidth={1.8} aria-hidden="true" className="text-muted-foreground" />
        {bits.join(" · ")}
      </span>
      {features.length > 0 && (
        <ul className="flex flex-wrap gap-1.5" aria-label="Database features">
          {features.map(([f, v]) => (
            <li key={f} title={v.reason ?? v.engine ?? undefined} className={`inline-flex items-center gap-1.5 rounded-full border px-2 py-0.5 ${v.state === "active" ? "border-success/40 bg-success/5 text-success" : v.state === "off" ? "border-border text-muted-foreground/80" : "border-border"}`}>
              <span aria-hidden="true" className={`size-1.5 rounded-full ${v.state === "active" ? "bg-success" : v.state === "off" ? "bg-muted-foreground/40" : "bg-muted-foreground"}`} />
              {f}: {v.state === "active" && v.engine ? v.engine : (STATE_TEXT[v.state] ?? v.state)}
            </li>
          ))}
        </ul>
      )}
      {data.backend === "postgres" && (
        <Link to={TOPOLOGY_PATH} data-slot="topology-link" className="text-primary hover:underline" title="Primary, replicas and shards: add, promote and move them in System">
          View topology
        </Link>
      )}
      {children}
    </div>
  );
}
