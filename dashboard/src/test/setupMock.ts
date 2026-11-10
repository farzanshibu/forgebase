/**
 * Mock of the first-run setup API (docs/requirements/first-run-setup.md sections 2 and 3), built from the contract:
 * `GET /api/setup/status`, `POST /api/setup/admin`, `GET /api/system/features`, `PATCH /api/system/config`.
 */
import type { ServerFeature, FeatureSetting, SettingValue } from "../api";
import { envelope, type MockApi } from "./mockApi";

export const SETUP_TOKEN = "tok_Zm9yZ2ViYXNl";

const setting = (key: string, o: Partial<FeatureSetting> & Pick<FeatureSetting, "label" | "type" | "value">): FeatureSetting => ({
  key, help: `Help for ${o.label}.`, unit: null, options: null, default: o.value, source: "default", editable: true, restart_required: false, secret: false, ...o,
});

export function sampleFeatures(): ServerFeature[] {
  const en = (id: string, value = true, o: Partial<FeatureSetting> = {}) =>
    setting(`${id}.enabled`, { label: "Enabled", type: "bool", value, default: true, ...o });
  return [
    { id: "database", name: "Database", summary: "Collections, records and SQL.", always_on: true, can_disable: false, enabled: true, studio_path: "/database", settings: [] },
    { id: "auth", name: "Authentication", summary: "Administrators, users and sessions.", always_on: true, can_disable: false, enabled: true, studio_path: "/auth", settings: [] },
    {
      id: "storage", name: "Object storage", summary: "Buckets and files.", always_on: false, can_disable: true, enabled: true, studio_path: "/storage",
      settings: [
        en("storage"),
        setting("storage.max_object_bytes", { label: "Largest object", type: "int", unit: "bytes", value: 104857600 }),
        setting("storage.allowed_types", { label: "Allowed types", type: "list", value: ["image/png", "image/jpeg"], source: "file" }),
      ],
    },
    {
      id: "jobs", name: "Background jobs", summary: "Queues, workers and schedules.", always_on: false, can_disable: true, enabled: true, studio_path: "/jobs",
      settings: [en("jobs"), setting("jobs.poll_interval", { label: "Poll interval", type: "duration", value: "5s" })],
    },
    { id: "billing", name: "Billing", summary: "Plans and invoices.", always_on: false, can_disable: true, enabled: false, studio_path: "/billing", settings: [en("billing", false, { source: "file", value: false })] },
    {
      id: "resp", name: "Redis protocol", summary: "A Redis-compatible listener.", always_on: false, can_disable: true, enabled: true, studio_path: "/redis",
      settings: [en("resp", true, { source: "env", editable: false }), setting("resp.bind", { label: "Listen address", type: "string", value: "127.0.0.1:6380" })],
    },
    {
      id: "server", name: "Server", summary: "Address, logging and storage location.", always_on: true, can_disable: false, enabled: true, studio_path: null,
      settings: [
        setting("server.public_url", { label: "Public URL", type: "url", value: "http://127.0.0.1:8090" }),
        setting("log.level", { label: "Log level", type: "enum", options: ["error", "warn", "info", "debug"], value: "warn", source: "flag", editable: false }),
        setting("server.mode", { label: "Mode", type: "enum", options: ["dev", "prod"], value: "dev" }),
        setting("server.master_key", { label: "Master key", type: "string", value: null, secret: true, editable: false, help: "Set the FORGEBASE_MASTER_KEY environment variable." }),
      ],
    },
  ];
}

export interface SetupState {
  required: boolean;
  features: ServerFeature[];
  restartPending: boolean;
  restartRequired: boolean;
  patches: { set: Record<string, SettingValue> }[];
  created: { token: string; email: string; password: string }[];
  /** Keys the next PATCH rejects with `validation_failed`. */
  rejectKeys: Record<string, string>;
}

/** Teach `mock` the setup and feature routes. Call before `signedIn` so these answers win over its defaults. */
export function installSetup(mock: MockApi, init: Partial<SetupState> = {}): SetupState {
  const st: SetupState = { required: false, features: sampleFeatures(), restartPending: false, restartRequired: false, patches: [], created: [], rejectKeys: {}, ...init };
  const snapshot = () => ({ config_file: "/srv/forge/forgebase.toml", restart_pending: st.restartPending, features: st.features });
  mock.on("GET", "/api/setup/status", () => ({ status: 200, body: { setup_required: st.required } }));
  mock.on("POST", "/api/setup/admin", ({ body }) => {
    const b = body as { token: string; email: string; password: string };
    if (!st.required) return envelope(409, "setup_complete", "setup is already complete");
    if (b.token !== SETUP_TOKEN) return envelope(403, "invalid_setup_token", "invalid setup token");
    if (b.password.length < 8) return envelope(400, "validation_failed", "validation failed", { password: "must be at least 8 characters" });
    st.created.push(b);
    st.required = false;
    mock.cookieSession = true;
    return { status: 201, body: { token_type: "Bearer", expires_in: 900, session_id: "s1", csrf_token: "csrf-1", session_expires_at: "2026-11-12T00:00:00Z", subject: { type: "admin", id: "adm1" } } };
  });
  mock.on("GET", "/api/system/features", () => (mock.cookieSession ? { status: 200, body: snapshot() } : envelope(401, "unauthenticated", "authentication required")));
  mock.on("PATCH", "/api/system/config", ({ body }) => {
    const set = (body as { set: Record<string, SettingValue> }).set;
    st.patches.push({ set });
    const all = st.features.flatMap((f) => f.settings);
    const bad = Object.keys(st.rejectKeys).filter((k) => k in set);
    if (bad.length > 0) return envelope(400, "validation_failed", "validation failed", Object.fromEntries(bad.map((k) => [k, st.rejectKeys[k]])));
    for (const k of Object.keys(set)) {
      const s = all.find((x) => x.key === k);
      if (!s) return envelope(400, "validation_failed", "validation failed", { [k]: "unknown setting" });
      if (s.source === "env" || s.source === "flag") return envelope(409, "setting_overridden", `${k} is set by ${s.source}`);
    }
    for (const f of st.features) {
      for (const s of f.settings) if (s.key in set) { s.value = set[s.key]!; s.source = "file"; }
      f.enabled = f.always_on ? true : (f.settings.find((s) => s.key === `${f.id}.enabled`)?.value as boolean | undefined) ?? f.enabled;
    }
    if (st.restartRequired) st.restartPending = true;
    return { status: 200, body: { written: "/srv/forge/forgebase.toml", restart_required: st.restartRequired, applied: Object.keys(set), features: st.features } };
  });
  return st;
}
