import { get, request, send } from "./client";
import type { LoginSession } from "./session";

/** `GET /api/setup/status`: the server is in setup mode while no administrator exists. */
export interface SetupStatus {
  setup_required: boolean;
}

export type SettingType = "bool" | "int" | "string" | "enum" | "duration" | "list" | "url";
export type SettingSource = "default" | "file" | "env" | "flag";
export type SettingValue = boolean | number | string | string[] | null;

export interface FeatureSetting {
  key: string;
  label: string;
  help: string;
  type: SettingType;
  unit?: string | null;
  options?: string[] | null;
  value: SettingValue;
  default: SettingValue;
  source: SettingSource;
  editable: boolean;
  restart_required: boolean;
  secret: boolean;
}

export interface ServerFeature {
  id: string;
  name: string;
  summary: string;
  always_on: boolean;
  can_disable: boolean;
  enabled: boolean;
  studio_path?: string | null;
  docs?: string | null;
  settings: FeatureSetting[];
}

/** `GET /api/system/features` (administrator only). */
export interface FeaturesResponse {
  config_file: string;
  restart_pending: boolean;
  features: ServerFeature[];
}

/** `PATCH /api/system/config` answer. */
export interface ConfigWritten {
  written: string;
  restart_required: boolean;
  applied: string[];
  features: ServerFeature[];
}

export const getSetupStatus = (signal?: AbortSignal) => request<SetupStatus>("/api/setup/status", { signal });

/** Creates the first administrator; answers like an administrator sign-in and sets the session cookie. */
export const createSetupAdmin = (token: string, email: string, password: string) =>
  send<LoginSession>("POST", "/api/setup/admin", { token, email, password });

export const getFeatures = (signal?: AbortSignal) => get<FeaturesResponse>("/api/system/features", signal);

/** One atomic write of the changed dotted keys. */
export const patchConfig = (set: Record<string, SettingValue>) => send<ConfigWritten>("PATCH", "/api/system/config", { set });
