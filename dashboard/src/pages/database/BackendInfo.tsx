/**
 * BackendInfo: which database engine this server runs on, in one compact line under the Database header.
 * It is the slot FB-155 (Database Studio for PostgreSQL) fills: the element has `data-slot="database-backend"`,
 * and `children` render after the summary, so FB-155 can add its topology and extension controls here.
 *
 * Feature states (FB-161): `active` runs on the PostgreSQL extension, `portable` on ForgeBase's own code, `off` is
 * switched off in Features and settings.
 */
import type { ReactNode } from "react";
import { useLoad } from "../../hooks";
import { BACKEND_NAME, getDatabaseInfo, type DatabaseFeature } from "./info";

const STATE_TEXT: Record<DatabaseFeature["state"], string> = { active: "on the extension", portable: "portable", off: "off" };

export default function BackendInfo({ children }: { children?: ReactNode }) {
  const { data } = useLoad(getDatabaseInfo, "database-info");
  // A failed or absent answer hides the line: the studio works without it.
  if (!data) return <div data-slot="database-backend">{children}</div>;
  const name = BACKEND_NAME[data.backend] ?? data.backend;
  const features = Object.entries(data.features ?? {});
  const bits = [
    data.server_version ? `${name} ${data.server_version}` : name,
    data.mode ? (data.mode === "managed" ? "bundled engine" : "external server") : undefined,
    typeof data.connections === "number" ? `${data.connections} connections` : undefined,
  ].filter(Boolean);
  return (
    <div data-slot="database-backend" aria-label="Database backend" className="mt-3 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
      <span className="font-medium text-foreground">{bits.join(" · ")}</span>
      {features.length > 0 && (
        <ul className="flex flex-wrap gap-1.5" aria-label="Database features">
          {features.map(([f, v]) => (
            <li key={f} title={v.reason ?? v.engine ?? undefined} className={`rounded-full border px-2 py-0.5 ${v.state === "active" ? "border-success/40 text-success" : "border-border"}`}>
              {f}: {v.state === "active" && v.engine ? v.engine : (STATE_TEXT[v.state] ?? v.state)}
            </li>
          ))}
        </ul>
      )}
      {children}
    </div>
  );
}
