/** Typed client for `/api/v1/system/backups` (snapshots, verify, prune; ADR-0024). Restore is CLI only. */
import { request, send } from "./client";

const B = "/api/v1/system/backups";
const enc = encodeURIComponent;

export interface BackupFile { path: string; size: number; sha256: string; kind: "database" | "storage" | "index" | "key" }
export interface BackupManifest {
  format: number; id: string; kind: string; label?: string | null; created: string; forgebase_version: string; schema_version: number;
  includes_indexes: boolean; master_key: { included: boolean; fingerprint?: string | null }; files: BackupFile[];
}
export interface BackupSnapshot { id: string; manifest: BackupManifest }
export interface Retention { keep_last?: number | null; max_age_days?: number | null }
export interface NewBackup { label?: string; include_indexes?: boolean; retention?: Retention }
export interface VerifyReport {
  ok: boolean; manifest?: BackupManifest | null; files_checked: number; bytes_checked: number; problems: string[]; warnings: string[];
}

export const BACKUP_OPERATIONS = [
  "get /api/v1/system/backups", "post /api/v1/system/backups", "get /api/v1/system/backups/{id}", "delete /api/v1/system/backups/{id}",
  "post /api/v1/system/backups/{id}/verify", "post /api/v1/system/backups/prune",
] as const;

export const listBackups = async (signal?: AbortSignal) => (await request<{ items: BackupSnapshot[] }>(B, { signal })).items;
export const createBackup = (b: NewBackup) => send<BackupManifest>("POST", B, b);
export const verifyBackup = (id: string) => send<VerifyReport>("POST", `${B}/${enc(id)}/verify`);
export const deleteBackup = (id: string) => send<void>("DELETE", `${B}/${enc(id)}`);
export const pruneBackups = (r: Retention) => send<{ deleted: string[] }>("POST", `${B}/prune`, r);

/** Total size of a snapshot's files, in bytes. */
export const backupSize = (s: BackupSnapshot) => s.manifest.files.reduce((n, f) => n + f.size, 0);
