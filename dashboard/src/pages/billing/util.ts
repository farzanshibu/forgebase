import { when } from "../functions/util";

export const THEAD = "text-xs uppercase tracking-wide text-zinc-500";
export const TABLE = "w-full text-left text-sm";
export const ROW = "border-t border-zinc-800";
export const FORM_GRID = "grid gap-3 sm:grid-cols-2 lg:grid-cols-3";
export const when_ = when;
export const idempotencyKey = (prefix: string) => `${prefix}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
