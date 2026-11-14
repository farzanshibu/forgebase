/**
 * `GET /api/system/capabilities` (ADR-0034, docs/api.md §11.5) and the `tls` section of `GET /api/system/info`
 * (ADR-0033): which engine runs each feature, and the state of the server's certificate.
 */
import { get } from "./client";

export type CapabilityState = "active" | "portable" | "off" | "native" | "unavailable";

export interface Capability {
  /** Stable name: `vector`, `graph`, `opencypher`, `fulltext`, `fuzzy`, `queue`, `cron`, `kv`, `realtime`, `read_replicas`, `sharding`. */
  feature: string;
  label: string;
  /** What it runs on: an extension and its version (`pgvector 0.8.7`) or the portable or built-in engine. */
  engine: string;
  /** True when `engine` is a PostgreSQL extension (an accelerator). */
  accelerated: boolean;
  state: CapabilityState;
  /** `[database.postgres.extensions]` mode of an accelerated feature on PostgreSQL. */
  mode: "auto" | "on" | "off" | null;
  extension: string | null;
  /** Why the feature is not on its accelerator although it could be, or why it is unavailable. */
  reason: string | null;
  limits: Record<string, unknown>;
}
export interface Capabilities { backend: string; features: Capability[] }

export const getCapabilities = (signal?: AbortSignal) => get<Capabilities>("/api/system/capabilities", signal);

/** The certificate the server serves (TLS mode on). `expires_in_secs` is negative once it has expired. */
export interface TlsCertificate {
  subject: string;
  names: string[];
  issuer: string;
  serial: string;
  not_before: string;
  not_after: string;
  expires_in_secs: number;
}
/** `info.tls`: `{ mode: "off" }` without TLS; the files or acme fields only in their mode. */
export interface TlsStatus {
  mode: "off" | "files" | "acme" | string;
  min_version?: string;
  alpn?: string[];
  hsts_max_age_secs?: number;
  http_listener?: string | null;
  resp?: boolean;
  certificate?: TlsCertificate | null;
  last_error?: string | null;
  last_error_at?: string | null;
  // files mode
  cert_file?: string;
  key_file?: string;
  reload_interval_secs?: number;
  last_reload?: string | null;
  // acme mode
  domains?: string[];
  directory_url?: string;
  challenges?: string[];
  last_renewal?: string | null;
  next_renewal?: string | null;
  renewal_failures?: number;
}
