/**
 * Fakes for the System studio's ops screens: `/api/system/capabilities` (ADR-0034), the `tls` section of
 * `/api/system/info` (ADR-0033) and the features list the tracing card reads. Custom routes, not contract-checked.
 */
import type { Capabilities, Capability, ServerFeature, TlsStatus } from "../api";
import { sampleFeatures } from "./setupMock";
import type { MockApi } from "./mockApi";

export const cap = (o: Partial<Capability> & Pick<Capability, "feature" | "label">): Capability => ({
  engine: "portable", accelerated: false, state: "portable", mode: null, extension: null, reason: null, limits: {}, ...o,
});

export const MATRIX: Capabilities = {
  backend: "postgres",
  features: [
    cap({ feature: "vector", label: "Vector search", engine: "pgvector 0.8.7", accelerated: true, state: "active", mode: "auto", extension: "vector", limits: { max_dimensions: 2000, index_kinds: ["hnsw", "ivfflat"] } }),
    cap({ feature: "graph", label: "Graph", engine: "portable (recursive SQL)", state: "portable", mode: "auto", extension: "age", reason: "Apache AGE is not installed", limits: { max_depth: 10 } }),
    cap({ feature: "opencypher", label: "openCypher", engine: "portable read-only subset", state: "portable", reason: null, limits: { max_path_depth: 10, max_return_items: 100 } }),
    cap({ feature: "fuzzy", label: "Fuzzy search", engine: "pg_trgm 1.6", accelerated: true, state: "active", mode: "on", extension: "pg_trgm" }),
    cap({ feature: "cron", label: "Schedules", engine: "pg_cron", state: "off", mode: "off", extension: "pg_cron", reason: "switched off in [database.postgres.extensions]" }),
    cap({ feature: "kv", label: "Key-value", engine: "built-in", state: "native" }),
    cap({ feature: "sharding", label: "Sharding", engine: "—", state: "unavailable", reason: "max_prepared_transactions is 0" }),
  ],
};

export const tlsFiles = (o: Partial<TlsStatus> = {}): TlsStatus => ({
  mode: "files", min_version: "1.2", alpn: ["h2", "http/1.1"], hsts_max_age_secs: 31_536_000, http_listener: null, resp: false,
  certificate: { subject: "forgebase.example.com", names: ["forgebase.example.com", "www.example.com"], issuer: "R11 (Let's Encrypt)", serial: "0a1b", not_before: "2026-10-01T00:00:00Z", not_after: "2027-01-01T00:00:00Z", expires_in_secs: 48 * 86_400 },
  last_error: null, last_error_at: null, cert_file: "/etc/forge/cert.pem", key_file: "/etc/forge/key.pem", reload_interval_secs: 300, last_reload: "2026-11-14T09:00:00Z", ...o,
});

export const tlsAcme = (o: Partial<TlsStatus> = {}): TlsStatus => ({
  mode: "acme", min_version: "1.2", alpn: ["h2", "http/1.1", "acme-tls/1"], hsts_max_age_secs: 0, http_listener: "0.0.0.0:80", resp: false,
  certificate: { subject: "forgebase.example.com", names: ["forgebase.example.com"], issuer: "R11 (Let's Encrypt)", serial: "0a1b", not_before: "2026-10-01T00:00:00Z", not_after: "2027-01-01T00:00:00Z", expires_in_secs: 60 * 86_400 },
  last_error: null, last_error_at: null, domains: ["forgebase.example.com", "api.example.com"], directory_url: "https://acme-v02.api.letsencrypt.org/directory", challenges: ["tls-alpn-01", "http-01"],
  last_renewal: "2026-11-01T00:00:00Z", next_renewal: "2026-12-01T00:00:00Z", renewal_failures: 0, ...o,
});

export interface SystemOps {
  capabilities: Capabilities;
  tls: TlsStatus | undefined;
  features: ServerFeature[] | undefined;
}

/** Install `/health`, `/health/ready`, `/api/system/info` (with a `tls` section), the capability matrix and the feature list. Call after `signedIn`. */
export function installSystemOps(mock: MockApi, init: Partial<SystemOps> = {}): SystemOps {
  const st: SystemOps = { capabilities: MATRIX, tls: tlsFiles(), features: sampleFeatures(), ...init };
  mock.on("GET", "/health", () => ({ status: 200, body: { status: "ok" } }));
  mock.on("GET", "/health/ready", () => ({ status: 200, body: { status: "ready", checks: { database: "ok" } } }));
  mock.on("GET", "/api/system/info", () => ({
    status: 200,
    body: { name: "forgebase", version: "0.3.5", mode: "embedded", data_dir: "/data", started_at: "2026-11-14T08:00:00Z", uptime_secs: 3600, database: { backend: "postgres", path: "/data/pg", size_bytes: 2048, journal_mode: "", schema_version: 56, capabilities: st.capabilities.features }, ...(st.tls ? { tls: st.tls } : {}) },
  }));
  mock.on("GET", "/api/system/capabilities", () => ({ status: 200, body: st.capabilities }));
  mock.on("GET", "/api/system/features", () => (st.features ? { status: 200, body: { config_file: "/srv/forge/forgebase.toml", restart_pending: false, features: st.features } } : { status: 404, body: { error: { code: "not_found", message: "no", request_id: "r" } } }));
  return st;
}
