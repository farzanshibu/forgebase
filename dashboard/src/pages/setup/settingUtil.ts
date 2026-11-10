import type { FeatureSetting, SettingValue } from "../../api";

/** Why a setting cannot be edited here, or null when it can. */
export function readOnlyReason(s: FeatureSetting): string | null {
  if (s.secret) return null;
  if (s.source === "env") return "Set by an environment variable, so the config file cannot override it. Change it where the server is started.";
  if (s.source === "flag") return "Set by a command-line flag, so the config file cannot override it. Change it in the server's start command.";
  if (!s.editable) return "This setting cannot be changed here.";
  return null;
}

const same = (a: unknown, b: unknown) => JSON.stringify(a ?? null) === JSON.stringify(b ?? null);

/** True when `v` is what the server already has (an empty text equals no value). */
export function isUnchanged(s: FeatureSetting, v: SettingValue): boolean {
  if (typeof v === "string" && s.value === null) return v === "";
  return same(v, s.value);
}

