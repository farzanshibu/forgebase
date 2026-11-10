/**
 * The logic behind Overview's panels, kept apart from the components so it can be unit tested and fast refresh keeps
 * working: the Getting started steps, the first-query snippet and the words for audit events.
 */
import type { AuditEntry } from "../../api";
import { explain } from "../../errors";
import { collection as collectionSnippet, type Snippet } from "../../snippets";
import { deployedFunctions, liveKeys, newestCollection, userCollections, type Probe, type ProjectState } from "./state";

// ---- Getting started

export type StepId = "admin" | "collection" | "key" | "file" | "function";

export interface Step {
  id: StepId;
  title: string;
  /** true done, false to do, undefined when the server could not tell us. */
  done: boolean | undefined;
  /** One plain sentence: what it is (to do), what exists (done) or why we could not check. */
  detail: string;
  to: string;
  action: string;
}

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;
const enc = encodeURIComponent;

function unknown<T>(p: Probe<T>): string | undefined {
  return p.state === "error" ? `Could not check: ${explain(p.error).title.replace(/\.$/, "")}.` : undefined;
}

/**
 * The checklist for a project. `disabled(studioPath)` hides a step whose studio the server switched off (the nav
 * hides it too), so a server without storage never asks for an upload.
 */
export function gettingStartedSteps(project: ProjectState, opts: { adminEmail?: string; disabled?: (studioPath: string) => boolean } = {}): Step[] {
  const off = (path: string, p: Probe<unknown>) => p.state === "off" || !!opts.disabled?.(path);
  const steps: Step[] = [
    {
      id: "admin",
      title: "Create the administrator",
      done: true,
      detail: opts.adminEmail ? `Done: you are signed in as ${opts.adminEmail}.` : "Done: you are signed in.",
      to: "/auth/admins",
      action: "Administrators",
    },
  ];

  const c = project.collections;
  const mine = c.state === "ok" ? userCollections(c.value) : [];
  const newest = c.state === "ok" ? newestCollection(c.value) : undefined;
  steps.push(
    newest
      ? { id: "collection", title: "Create a collection", done: true, detail: `${plural(mine.length, "collection")}. The newest is ${newest.name}.`, to: `/database/c/${enc(newest.name)}`, action: `Open ${newest.name}` }
      : {
          id: "collection", title: "Create a collection", done: c.state === "ok" ? false : undefined,
          detail: unknown(c) ?? "A collection is a table with a schema. Give it a few fields, then add a record.",
          to: "/database/new", action: "New collection",
        },
  );

  const k = project.keys;
  const keys = k.state === "ok" ? liveKeys(k.value) : [];
  steps.push(
    keys.length > 0
      ? { id: "key", title: "Create an API key", done: true, detail: `${plural(keys.length, "active key")}. Your app sends one with each request.`, to: "/auth/keys", action: "Manage keys" }
      : {
          id: "key", title: "Create an API key", done: k.state === "ok" ? false : undefined,
          detail: unknown(k) ?? "Your app sends an API key with each request. Give it only the access it needs.",
          to: "/auth/keys?new=1", action: "Create API key",
        },
  );

  const s = project.storage;
  if (!off("storage", s)) {
    const stats = s.state === "ok" ? s.value : undefined;
    steps.push(
      stats && stats.objects > 0
        ? { id: "file", title: "Upload a file", done: true, detail: `${plural(stats.objects, "file")} in ${plural(stats.buckets, "bucket")}.`, to: "/storage", action: "Open Storage" }
        : stats && stats.buckets > 0
          ? { id: "file", title: "Upload a file", done: false, detail: "Open a bucket and drop a file into it, such as an avatar or an export.", to: "/storage", action: "Upload a file" }
          : {
              id: "file", title: "Upload a file", done: stats ? false : undefined,
              detail: unknown(s) ?? "Files live in buckets. Make a bucket, then drop a file into it.",
              to: "/storage?new=1", action: "New bucket",
            },
    );
  }

  const f = project.functions;
  if (!off("functions", f)) {
    const live = f.state === "ok" ? deployedFunctions(f.value) : [];
    const latest = live.reduce<(typeof live)[number] | undefined>((b, x) => (!b || x.updated_at > b.updated_at ? x : b), undefined);
    steps.push(
      latest
        ? { id: "function", title: "Deploy a function", done: true, detail: `${plural(live.length, "function")} deployed. The newest is ${latest.name}.`, to: `/functions/f/${enc(latest.name)}`, action: `Open ${latest.name}` }
        : {
            id: "function", title: "Deploy a function", done: f.state === "ok" ? false : undefined,
            detail: unknown(f) ?? "Write a function in the browser that reads your collection, deploy it and test it.",
            to: "/functions/new", action: "New function",
          },
    );
  }
  return steps;
}

// ---- Connect your app

/** The name the snippet uses while the project has no collection. */
export const PLACEHOLDER_COLLECTION = "posts";

/** Install, connect and list the first page of a collection, in JavaScript, Rust and curl. */
export function firstQuerySnippet(url: string, name: string): Snippet {
  const list = collectionSnippet(url, name);
  return {
    js: `// Install: npm install @forgebase/sdk\n${list.js}`,
    rust: `// Install: cargo add forgebase-sdk tokio serde_json\n${list.rust}`,
    curl: `# List the newest records of ${name}\n${list.curl}`,
  };
}

// ---- Recent activity

/** Plain words for the events a new project sees most; anything else shows its event name. */
export const EVENT_WORDS: Record<string, string> = {
  login: "Signed in",
  "login.admin": "Administrator sign-in",
  logout: "Signed out",
  "admin.created": "Administrator created",
  "admin.deleted": "Administrator deleted",
  "admin.password_reset": "Administrator password reset",
  "signing_key.rotated": "Signing key rotated",
  "api_key.created": "API key created",
  "api_key.revoked": "API key revoked",
  "pat.created": "Personal access token created",
  "service_account.created": "Service account created",
  "user.created": "User created",
  "user.deleted": "User deleted",
  "mfa.enabled": "Two-step verification turned on",
  "mfa.disabled": "Two-step verification turned off",
};

export function eventWords(e: Pick<AuditEntry, "event" | "outcome">): string {
  const base = EVENT_WORDS[e.event] ?? e.event.replace(/[._]/g, " ").replace(/^./, (c) => c.toUpperCase());
  return e.outcome && e.outcome !== "success" ? `${base} (${e.outcome})` : base;
}

/** "cli", "system", "anonymous", "admin:<id>" → words. */
export function actorWords(actor: string, adminEmail?: string, adminId?: string): string {
  if (actor === "cli") return "command line";
  if (actor === "system") return "server";
  if (actor === "anonymous") return "someone not signed in";
  if (adminId && actor === `admin:${adminId}`) return adminEmail ? `you (${adminEmail})` : "you";
  return actor;
}

/** "just now", "5 min ago", "3 h ago", else the date. */
export function ago(iso: string, now = Date.now()): string {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return iso;
  const s = Math.max(0, Math.round((now - t) / 1000));
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  return new Date(t).toLocaleDateString();
}
