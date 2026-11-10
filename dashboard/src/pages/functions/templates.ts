import type { FunctionLanguage, HookSpec } from "../../api";

export type TemplateId = "http" | "webhook" | "hook" | "job" | "scheduled";

export interface Template {
  id: TemplateId;
  label: string;
  description: string;
  /** Capabilities the template needs (the person can change them). */
  capabilities: string[];
  hooks: HookSpec[];
  /** Source in each language; no secrets or real data. */
  source: Record<FunctionLanguage, string>;
}

const http = (ts: boolean) => `import { serve } from "forgebase";

serve(async (req${ts ? ": Request" : ""}, ctx) => {
  // ctx.auth is the caller ({ id, type, roles, ... }) or null when anonymous.
  const url = new URL(req.url);
  const name = url.searchParams.get("name") ?? "world";
  return Response.json({ message: \`Hello, \${name}!\`, caller: ctx.auth?.id ?? null });
});
`;

const webhook = (ts: boolean) => `import { serve } from "forgebase";

// Checks an HMAC-SHA256 signature of the raw body. Store the shared secret as the function secret WEBHOOK_SECRET.
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
  return Response.json({ ok: true });
});
`;

const hook = (ts: boolean) => `import { serve } from "forgebase";

// Runs after a record is created or updated (configure the collection under Record hooks).
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

export const TEMPLATES: Template[] = [
  { id: "http", label: "HTTP API", description: "Answers HTTP requests with JSON.", capabilities: ["log"], hooks: [], source: { typescript: http(true), javascript: http(false) } },
  { id: "webhook", label: "Webhook with signature check", description: "Verifies an HMAC signature before handling the request.", capabilities: ["log", "env", "secrets"], hooks: [], source: { typescript: webhook(true), javascript: webhook(false) } },
  { id: "hook", label: "Database after-hook", description: "Reacts to record changes.", capabilities: ["log", "kv"], hooks: [{ collection: "orders", events: ["after_create", "after_update"] }], source: { typescript: hook(true), javascript: hook(false) } },
  { id: "job", label: "Background job", description: "Runs from the jobs queue.", capabilities: ["log", "kv", "jobs.enqueue"], hooks: [], source: { typescript: job(true), javascript: job(false) } },
  { id: "scheduled", label: "Scheduled job", description: "Runs on a schedule through a recurring job.", capabilities: ["log", "kv"], hooks: [], source: { typescript: scheduled(true), javascript: scheduled(false) } },
];

export const template = (id: TemplateId): Template => TEMPLATES.find((t) => t.id === id) ?? TEMPLATES[0]!;
