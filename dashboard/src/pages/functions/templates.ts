import type { FunctionLanguage, HookSpec } from "../../api";

export type TemplateId = "read" | "http" | "webhook" | "hook" | "job" | "scheduled";

export interface Template {
  id: TemplateId;
  label: string;
  description: string;
  /** Capabilities the template needs (the person can change them). */
  capabilities: string[];
  hooks: HookSpec[];
  /** True when the template works on one collection: the person picks it (never an assumed name like `orders`). */
  needsCollection?: boolean;
  /** Source in each language for the chosen collection (ignored by templates that need none); no secrets or real data. */
  source: (collection: string) => Record<FunctionLanguage, string>;
}

const q = (s: string) => JSON.stringify(s);

const http = (ts: boolean) => `import { serve } from "forgebase";

serve(async (req${ts ? ": Request" : ""}, ctx) => {
  // ctx.auth is the caller ({ id, type, roles, ... }) or null when anonymous.
  const url = new URL(req.url);
  const name = url.searchParams.get("name") ?? "world";
  return Response.json({ message: \`Hello, \${name}!\`, caller: ctx.auth?.id ?? null });
});
`;

const read = (ts: boolean, c: string) => `import { serve } from "forgebase";

// Lists the newest records of ${c}. Needs the db.read capability. Try ?perPage=5.
serve(async (req${ts ? ": Request" : ""}, ctx) => {
  const perPage = Number(new URL(req.url).searchParams.get("perPage") ?? "10");
  const { items } = await ctx.db.from(${q(c)}).list({ perPage });
  console.log("read", items.length, "records of", ${q(c)});
  return Response.json({ collection: ${q(c)}, count: items.length, items });
});
`;

const webhook = (ts: boolean, c: string) => `import { serve } from "forgebase";

// Checks an HMAC-SHA256 signature of the raw body, then saves it as a record of ${c}.
// Store the shared secret as the function secret WEBHOOK_SECRET. Needs db.write.
const hex = (buf${ts ? ": ArrayBuffer" : ""}) => [...new Uint8Array(buf)].map((b) => b.toString(16).padStart(2, "0")).join("");

async function sign(secret${ts ? ": string" : ""}, body${ts ? ": string" : ""})${ts ? ": Promise<string>" : ""} {
  const key = await crypto.subtle.importKey("raw", new TextEncoder().encode(secret), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  return hex(await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(body)));
}

serve(async (req${ts ? ": Request" : ""}, ctx) => {
  const secret = ctx.env.get("WEBHOOK_SECRET");
  if (!secret) return new Response("WEBHOOK_SECRET is not set", { status: 500 });
  const body = await req.text();
  const given = req.headers.get("x-signature") ?? "";
  if (given !== (await sign(secret, body))) return new Response("bad signature", { status: 401 });
  console.log("webhook accepted", body.length, "bytes");
  const record = await ctx.db.from(${q(c)}).create({ payload: body });
  return Response.json({ ok: true, id: record.id });
});
`;

const hook = (ts: boolean, c: string) => `import { serve } from "forgebase";

// Runs after a record of ${c} is created or updated (the version lists ${c} under Record hooks).
serve(async (req${ts ? ": Request" : ""}, ctx) => {
  const event = await req.json();
  console.info("record event", event.event, event.collection, event.record?.id);
  // Example: count events in the function's private key-value namespace.
  await ctx.kv.incr(\`events:\${event.collection}\`);
  return new Response(null, { status: 204 });
});
`;

const job = (ts: boolean) => `import { serve } from "forgebase";

// Runs as a background job: enqueue it with ctx.jobs.enqueue("function.invoke", { function: "<name>", ... }).
serve(async (req${ts ? ": Request" : ""}, ctx) => {
  const payload = await req.json();
  console.log("job payload", JSON.stringify(payload));
  await ctx.kv.set("last-job", JSON.stringify(payload), { ttlMs: 3600_000 });
  return Response.json({ done: true });
});
`;

const scheduled = (ts: boolean) => `import { serve } from "forgebase";

// A scheduled job: create a recurring job of kind function.invoke under Jobs, pointing at this function.
serve(async (_req${ts ? ": Request" : ""}, ctx) => {
  const stamp = new Date().toISOString();
  console.log("tick", stamp);
  await ctx.kv.set("last-tick", stamp);
  return Response.json({ ranAt: stamp });
});
`;

const both = (f: (ts: boolean, c: string) => string) => (c: string): Record<FunctionLanguage, string> => ({ typescript: f(true, c), javascript: f(false, c) });
const plain = (f: (ts: boolean) => string) => (): Record<FunctionLanguage, string> => ({ typescript: f(true), javascript: f(false) });

export const TEMPLATES: Template[] = [
  { id: "read", label: "Read a collection", description: "Returns the newest records of a collection as JSON.", capabilities: ["log", "db.read"], hooks: [], needsCollection: true, source: both(read) },
  { id: "http", label: "HTTP API", description: "Answers HTTP requests with JSON.", capabilities: ["log"], hooks: [], source: plain(http) },
  { id: "webhook", label: "Webhook with signature check", description: "Verifies an HMAC signature, then saves the body to a collection.", capabilities: ["log", "env", "secrets", "db.write"], hooks: [], needsCollection: true, source: both(webhook) },
  { id: "hook", label: "Database after-hook", description: "Reacts to changes of one collection.", capabilities: ["log", "kv"], hooks: [], needsCollection: true, source: both(hook) },
  { id: "job", label: "Background job", description: "Runs from the jobs queue.", capabilities: ["log", "kv", "jobs.enqueue"], hooks: [], source: plain(job) },
  { id: "scheduled", label: "Scheduled job", description: "Runs on a schedule through a recurring job.", capabilities: ["log", "kv"], hooks: [], source: plain(scheduled) },
];

export const template = (id: TemplateId): Template => TEMPLATES.find((t) => t.id === id) ?? TEMPLATES[0]!;

/** The after-hooks a template configures for the chosen collection. */
export const hooksFor = (t: Template, collection: string): HookSpec[] => (t.id === "hook" && collection ? [{ collection, events: ["after_create", "after_update"] }] : t.hooks);
