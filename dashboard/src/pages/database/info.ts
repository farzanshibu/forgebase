/** `GET /api/system/info` database part (FB-148, FB-161). Typed here because api/types.ts only knows the SQLite fields. */
import { get } from "../../api/client";

export type FeatureState = "active" | "portable" | "off";

export interface DatabaseFeature {
  /** The engine it runs on: `pgvector 0.8.7` or `portable`. */
  engine?: string;
  state: FeatureState;
  mode?: string;
  extension?: string | null;
  reason?: string | null;
}

export interface DatabaseInfo {
  /** `sqlite` or `postgres`. */
  backend: string;
  /** `managed` or `external` on PostgreSQL. */
  mode?: string | null;
  path?: string;
  size_bytes?: number;
  server_version?: string;
  connections?: number | null;
  engine?: { state: string; version: string } | null;
  /** The backend a new project of this build gets (FB-177): `postgres`, or `sqlite` in a `-lite` build. */
  default_backend?: string;
  /** This build embeds the PostgreSQL engine (not a `-lite` build). */
  bundled_postgres?: boolean;
  features?: Record<string, DatabaseFeature>;
}

export const getDatabaseInfo = async (signal?: AbortSignal): Promise<DatabaseInfo | undefined> => {
  const info = await get<{ database?: DatabaseInfo }>("/api/system/info", signal);
  return info.database;
};

export const BACKEND_NAME: Record<string, string> = { sqlite: "SQLite", postgres: "PostgreSQL" };

/** Where the topology screen lives: System studio, tab "Database topology" (FB-176). */
export const TOPOLOGY_PATH = "/system/topology";

/** What the CLI prints for an SQLite project in a build where PostgreSQL is the default (same words as `sqlite_notice`). */
export const SQLITE_NOTICE = "This project uses SQLite. PostgreSQL is the default now: `forgebase db convert --to postgres` moves it.";

/** True when the System studio should show the SQLite notice. */
export const showSqliteNotice = (db: Partial<Pick<DatabaseInfo, "backend" | "default_backend">> | undefined): boolean =>
  db?.backend === "sqlite" && db.default_backend === "postgres";
