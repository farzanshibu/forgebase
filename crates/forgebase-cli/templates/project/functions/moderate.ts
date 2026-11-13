/// <reference path="./forgebase.d.ts" />
import { serve } from "forgebase";

// A before-create hook on messages: rejects a message with a blocked word.
// A before-hook gets {event, collection, record_id, data, auth}; returning
// { allow: false, reason } makes the write fail with `400 hook_rejected`.
const BLOCKED = ["spam", "scam"];

serve(async (_req: Request, ctx) => {
  const ev = ctx.event as { data?: { body?: string } } | null;
  const body = (ev?.data?.body ?? "").toLowerCase();
  const hit = BLOCKED.find((w) => body.includes(w));
  if (hit) {
    return { allow: false, reason: `messages may not contain "${hit}"` };
  }
  return { allow: true };
});
