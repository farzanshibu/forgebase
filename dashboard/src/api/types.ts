/** Wire types for the ForgeBase API (docs/PLAN.md section 5). */

export interface ApiErrorBody {
  error: { code: string; message: string; request_id?: string };
}

export interface HealthResponse {
  status: string;
}

export interface ReadyResponse {
  status: string;
  checks: Record<string, string>;
}

export interface SystemInfo {
  name: string;
  version: string;
  mode: string;
  data_dir: string;
  started_at: string;
  uptime_secs: number;
  database: {
    path: string;
    size_bytes: number;
    journal_mode: string;
    schema_version: number;
  };
}
