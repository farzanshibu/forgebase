import { createContext, useContext } from "react";
import type { RealmConfig } from "../../api";

/** The realm every screen of the studio acts on. */
export const RealmCtx = createContext<string>("default");
export const useRealm = () => useContext(RealmCtx);

export const fmtTime = (ms: number | null | undefined) => (ms ? new Date(ms).toISOString().replace("T", " ").slice(0, 19) + "Z" : "never");
export const fmtIso = (s: string | null | undefined) => (s ? s.replace("T", " ").slice(0, 19) + "Z" : "never");
export const lines = (s: string) => s.split("\n").map((x) => x.trim()).filter(Boolean);
export const csv = (s: string) => s.split(",").map((x) => x.trim()).filter(Boolean);

export const defaultConfig = (): RealmConfig => ({
  signup: true, methods: { password: true, magic_link: true, email_otp: true, oauth: true, passkey: true, phone: false, ldap: false },
  require_verified_email: null, phone: { signup: true }, ldap: null, turnstile: null,
});

/** `name=value` lines to the variables object; a dotted name (`user.name`) becomes a nested object, as the template syntax reads it. */
export function nest(src: string): Record<string, never> {
  const out: Record<string, unknown> = {};
  for (const line of src.split("\n")) {
    const i = line.indexOf("=");
    if (i < 1) continue;
    const path = line.slice(0, i).trim().split(".");
    let cur = out;
    path.slice(0, -1).forEach((k) => { cur = (cur[k] = typeof cur[k] === "object" && cur[k] !== null ? cur[k] : {}) as Record<string, unknown>; });
    cur[path[path.length - 1]!] = line.slice(i + 1);
  }
  return out as Record<string, never>;
}
