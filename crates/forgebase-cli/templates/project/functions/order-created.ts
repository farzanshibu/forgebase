/// <reference path="./forgebase.d.ts" />
import { serve } from "forgebase";

// A record hook: runs after every order is created (see function.toml).
// The webhook URL is a secret: `forgebase project apply` reads it from the
// SLACK_WEBHOOK_URL environment variable; it is never stored in the project.
serve(async (_req: Request, ctx) => {
  if (ctx.trigger !== "hook") {
    return new Response("hook only", { status: 400 });
  }
  const ev = ctx.event as { collection: string; record: { id: string; total?: number } };
  console.log("order created", ev.record.id, ev.record.total ?? 0);
  const url = ctx.env.get("SLACK_WEBHOOK_URL");
  if (url && url.startsWith("https://")) {
    await fetch(url, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ text: `New order ${ev.record.id}` }),
    });
  }
  return { ok: true };
});
