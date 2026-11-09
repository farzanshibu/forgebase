import type { components, paths } from "./generated/openapi";

/** Types generated from the server's OpenAPI document (`npm run generate`). */
export type { components, paths };
export type Schemas = components["schemas"];
