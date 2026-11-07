import { ApiError, get } from "./client";
import type { HealthResponse, ReadyResponse, SystemInfo } from "./types";

export const getHealth = (signal?: AbortSignal) => get<HealthResponse>("/health", signal);

/**
 * `/health/ready` answers 503 with the failing check when not ready. Treat that
 * as data (not an exception) when the body carries a `checks` map.
 */
export async function getReady(signal?: AbortSignal): Promise<ReadyResponse> {
  try {
    return await get<ReadyResponse>("/health/ready", signal);
  } catch (err) {
    if (err instanceof ApiError && err.status === 503) {
      const b = err.body as Partial<ReadyResponse> | undefined;
      if (b && typeof b === "object" && b.checks) {
        return { status: b.status ?? "not_ready", checks: b.checks };
      }
    }
    throw err;
  }
}

export const getSystemInfo = (signal?: AbortSignal) => get<SystemInfo>("/api/system/info", signal);
