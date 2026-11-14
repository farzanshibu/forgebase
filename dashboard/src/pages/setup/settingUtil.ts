import type { FeatureSetting, ServerFeature, SettingValue } from "../../api";
import { driverOf as driverGroup } from "./storageDrivers";

/** Why a setting cannot be edited here, or null when it can. */
export function readOnlyReason(s: FeatureSetting): string | null {
  if (s.secret) return null;
  if (s.source === "env") return "Set by an environment variable, so the config file cannot override it. Change it where the server is started.";
  if (s.source === "flag") return "Set by a command-line flag, so the config file cannot override it. Change it in the server's start command.";
  if (!s.editable) return "This setting is fixed while the server runs. Edit the config file and restart the server to change it.";
  return null;
}

const same = (a: unknown, b: unknown) => JSON.stringify(a ?? null) === JSON.stringify(b ?? null);

/** True when `v` is what the server already has (an empty text equals no value). */
export function isUnchanged(s: FeatureSetting, v: SettingValue): boolean {
  if (typeof v === "string" && s.value === null) return v === "";
  return same(v, s.value);
}


export const enabledKey = (f: ServerFeature) => `${f.id}.enabled`;

/** The feature's own on/off setting, when it has one. */
export const enabledSetting = (f: ServerFeature): FeatureSetting | undefined => f.settings.find((s) => s.key === enabledKey(f));

const GROUP_TITLES: Record<string, string> = {
  postgres: "PostgreSQL",
  "postgres.extensions": "PostgreSQL extensions",
  replicas: "Read replicas",
  sharding: "Sharding",
  nodes: "Servers declared in the config file",
  s3: "Amazon S3 and compatible",
  gcs: "Google Cloud Storage",
  azure: "Azure Blob Storage",
};

const GROUP_RANK = ["", "postgres", "postgres.extensions", "replicas", "sharding", "nodes", "s3", "gcs", "azure"];
const groupRank = (g: string) => { const i = GROUP_RANK.indexOf(g); return i < 0 ? GROUP_RANK.length : i; };

/**
 * A feature's settings in groups. `database.postgres.pool_size` and `database.postgres.extensions.vector` sit in "PostgreSQL" and
 * "PostgreSQL extensions"; a key one level under the feature (`storage.max_object_bytes`) is in the unnamed first group.
 */
export function groupSettings(f: ServerFeature): { title: string | null; rows: FeatureSetting[] }[] {
  const groups = new Map<string, FeatureSetting[]>();
  for (const s of f.settings) {
    if (s.key === enabledKey(f)) continue;
    const rest = s.key.startsWith(`${f.id}.`) ? s.key.slice(f.id.length + 1).split(".") : [s.key];
    // `database.nodes` (a list of tables, FB-175) sits under the feature but is its own group: it belongs with replicas and shards.
    // The storage settings of one driver (`storage.gcs_bucket`) sit in a group named for it; the ones every driver shares stay in the first.
    const g = s.key === "database.nodes" ? "nodes" : f.id === "storage" && driverGroup(s.key) ? driverGroup(s.key)! : rest.slice(0, -1).join(".");
    groups.set(g, [...(groups.get(g) ?? []), s]);
  }
  return [...groups.entries()]
    // The unnamed group first, then PostgreSQL (the default backend) before anything else.
    .sort(([a], [b]) => groupRank(a) - groupRank(b))
    .map(([g, rows]) => ({ title: g === "" ? null : (GROUP_TITLES[g] ?? g.replace(/[._]/g, " ").replace(/^./, (c) => c.toUpperCase())), rows }));
}

/** The anchor of a feature's settings card, for the "Jump to" list on Features and settings. */
export const settingsAnchor = (featureId: string) => `settings-${featureId}`;
