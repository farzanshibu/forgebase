/**
 * Plain words for the Authentication and Access studios: times, principals, audit events and user agents.
 * Kept apart from common.tsx so that file only exports components (fast refresh).
 */
import { useMemo, useState } from "react";
import { listAdmins } from "../../api";
import { useAdminScope } from "../../auth/scope";
import { ErrorView } from "../../components/ui";
import { useLoad } from "../../hooks";
import { toDate, utcText } from "../../components/timeFormat";

/** The exact time in UTC: "2026-11-09 10:00:00Z". Relative times are `When` (components/Time). */
export const fmtTime = (v: string | number | null | undefined): string => {
  if (v === null || v === undefined || v === "") return "—";
  return toDate(v) ? utcText(v) : String(v);
};

/** Words for initials: "ann.lee@example.com" gives "ann lee", so the avatar reads "AL". */
export function initialsName(name: string): string {
  const local = name.includes("@") ? name.split("@")[0]! : name;
  return local.replace(/[._+-]+/g, " ").trim() || name;
}

const KIND_WORDS: Record<string, string> = {
  user: "User",
  admin: "Admin",
  service_account: "Service account",
  sa: "Service account",
  api_key: "API key",
  pat: "Token",
  function: "Function",
  anonymous: "Anonymous",
  system: "System",
};

/** "user:users/u1" as `{ kind: "user", label: "User", id: "users/u1" }`. */
export function principalParts(p: string): { kind: string; label: string; id: string } {
  const i = p.indexOf(":");
  if (i < 0) return { kind: "", label: "", id: p };
  const kind = p.slice(0, i);
  return { kind, label: KIND_WORDS[kind] ?? kind, id: p.slice(i + 1) };
}

/** Administrator ids to e-mail addresses, so an audit line names a person. Empty for a realm administrator. */
export function useAdminEmails(): Map<string, string> {
  const { platform } = useAdminScope();
  const admins = useLoad((s) => (platform ? listAdmins(s) : Promise.resolve([])), `admin-emails:${platform}`);
  return useMemo(() => new Map((admins.data ?? []).map((a) => [a.id, a.email])), [admins.data]);
}

const EVENTS: Record<string, string> = {
  login: "Signed in",
  "login.failed": "Sign-in failed",
  "login.admin": "Administrator signed in",
  "login.mfa_challenge": "Asked for a second factor",
  logout: "Signed out",
  signup: "Signed up",
  refresh: "Session refreshed",
  anonymous: "Anonymous sign-in",
  magic_link: "Signed in with a link",
  "magic_link.requested": "Sign-in link requested",
  "email_otp.requested": "E-mail code requested",
  "sms_otp.requested": "Text message code requested",
  "phone.verification_sent": "Phone check sent",
  "mfa.enable": "MFA turned on",
  "mfa.disable": "MFA turned off",
  "mfa.verify": "Second factor checked",
  "mfa.recovery_codes": "Recovery codes made",
  "password.change": "Password changed",
  "password_reset.requested": "Password reset requested",
  "verify_email.requested": "Verification e-mail sent",
  "verify_email.confirmed": "E-mail verified",
  "oauth.login": "Signed in with a provider",
  "oauth.token": "OAuth token issued",
  "oauth.client_auth": "OAuth client checked",
  "webauthn.login": "Signed in with a passkey",
  "webauthn.register": "Passkey added",
  "webauthn.clone_suspected": "Copied passkey suspected",
  "account.exported": "Account data exported",
  "setup.admin": "Administrator created at setup",
  "signing_key.rotate_requested": "Signing key rotation requested",
  "oidc.key_rotate_requested": "OIDC key rotation requested",
};

/** An audit event code in plain words: "login.failed" reads "Sign-in failed". */
export const eventLabel = (e: string) => EVENTS[e] ?? e.replace(/[._]+/g, " ").replace(/^./, (c) => c.toUpperCase());

/** Event prefixes worth suggesting in a filter box. */
export const EVENT_PREFIXES = ["login", "login.failed", "logout", "signup", "mfa.", "password", "magic_link", "oauth.", "webauthn.", "verify_email."];

/** A user agent in a few words: "Chrome on Windows", "curl", "Node.js". */
export function agentLabel(ua: string | null | undefined): string {
  if (!ua) return "Unknown client";
  if (/^curl\//i.test(ua)) return "curl";
  if (/^(node|undici)/i.test(ua)) return "Node.js";
  if (/forgebase/i.test(ua)) return "ForgeBase SDK";
  const browser = /Edg\//.test(ua) ? "Edge" : /OPR\//.test(ua) ? "Opera" : /Firefox\//.test(ua) ? "Firefox" : /Chrome\//.test(ua) ? "Chrome" : /Safari\//.test(ua) ? "Safari" : undefined;
  const os = /Windows/.test(ua) ? "Windows" : /iPhone|iPad/.test(ua) ? "iOS" : /Mac OS X/.test(ua) ? "macOS" : /Android/.test(ua) ? "Android" : /Linux/.test(ua) ? "Linux" : undefined;
  if (browser && os) return `${browser} on ${os}`;
  return browser ?? os ?? (ua.length > 32 ? `${ua.slice(0, 32)}…` : ua);
}

/** Runs an async action, keeping its error for display. */
export function useAction() {
  const [error, setError] = useState<unknown>();
  const [busy, setBusy] = useState(false);
  const run = async (fn: () => Promise<unknown>): Promise<boolean> => {
    setBusy(true);
    setError(undefined);
    try {
      await fn();
      return true;
    } catch (e) {
      setError(e);
      return false;
    } finally {
      setBusy(false);
    }
  };
  return { error, busy, run, errorView: <ErrorView error={error} /> };
}

