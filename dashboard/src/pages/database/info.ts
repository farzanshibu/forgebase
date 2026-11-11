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
  features?: Record<string, DatabaseFeature>;
}

export const getDatabaseInfo = async (signal?: AbortSignal): Promise<DatabaseInfo | undefined> => {
  const info = await get<{ database?: DatabaseInfo }>("/api/system/info", signal);
  return info.database;
};

export const BACKEND_NAME: Record<string, string> = { sqlite: "SQLite", postgres: "PostgreSQL" };
