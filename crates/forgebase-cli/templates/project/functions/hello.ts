/// <reference path="./forgebase.d.ts" />
import { serve } from "forgebase";

// An HTTP function: GET /api/v1/functions/hello/invoke?name=you
serve(async (req: Request, ctx) => {
  const name = new URL(req.url).searchParams.get("name") ?? "world";
  console.log("greeting", name);
  return Response.json({ message: `Hello, ${name}!`, caller: ctx.auth?.id ?? null });
});
