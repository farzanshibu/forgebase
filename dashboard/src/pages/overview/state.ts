/**
 * What the project already has, read from the server for Overview's Getting started checklist and Connect your app
 * panel (dashboard-ux.md, "Overview is the starting point"; audit A-02).
 *
 * Usage:
 *   const project = useProjectState();
 *   project.data?.collections   // { state: "ok", value: CollectionDef[] } | { state: "off" } | { state: "error", error }
 *
 * Each probe stands alone: a switched-off feature or a failed call never hides the other steps.
 */
import { useEffect } from "react";
import { ApiError, listApiKeys, listCollections, listFunctions, storageStats } from "../../api";
import type { ApiKeyView, CollectionDef, FunctionInfo, StorageStats } from "../../api";
import { useLoad, type LoadState } from "../../hooks";

/** One probe: a value, a feature the server does not offer (switched off or not built in), or a failure. */
export type Probe<T> = { state: "ok"; value: T } | { state: "off" } | { state: "error"; error: unknown };

export async function probe<T>(f: () => Promise<T>): Promise<Probe<T>> {
  try {
    return { state: "ok", value: await f() };
  } catch (e) {
    if (e instanceof DOMException && e.name === "AbortError") throw e;
    if (e instanceof ApiError && (e.code === "feature_disabled" || e.status === 404 || e.status === 501)) return { state: "off" };
    return { state: "error", error: e };
  }
}

export interface ProjectState {
  collections: Probe<CollectionDef[]>;
  keys: Probe<ApiKeyView[]>;
  storage: Probe<StorageStats>;
  functions: Probe<FunctionInfo[]>;
}

export async function loadProjectState(signal?: AbortSignal): Promise<ProjectState> {
  const [collections, keys, storage, functions] = await Promise.all([
    probe(() => listCollections(signal)),
    probe(() => listApiKeys(signal)),
    probe(() => storageStats(signal)),
    probe(() => listFunctions(signal)),
  ]);
  return { collections, keys, storage, functions };
}

/** Loads the project state now and again when the tab regains focus (the user may have created something elsewhere). */
export function useProjectState(): LoadState<ProjectState> {
  const state = useLoad(loadProjectState, "overview:project");
  const { reload } = state;
  useEffect(() => {
    const again = () => {
      if (document.visibilityState === "visible") reload();
    };
    document.addEventListener("visibilitychange", again);
    return () => document.removeEventListener("visibilitychange", again);
  }, [reload]);
  return state;
}

/**
 * Collections the user made: not the server's own (`system`, `_` names) and not the `users` auth collection every
 * new project starts with, so a fresh server shows "Create a collection" as still to do.
 */
export function userCollections(list: CollectionDef[]): CollectionDef[] {
  return list.filter((c) => !c.system && !c.name.startsWith("_") && !(c.name === "users" && c.type === "auth"));
}

/** The most recently created user collection, or undefined. */
export function newestCollection(list: CollectionDef[]): CollectionDef | undefined {
  const mine = userCollections(list);
  return mine.reduce<CollectionDef | undefined>((best, c) => (!best || (c.created ?? "") > (best.created ?? "") ? c : best), undefined);
}

/** Keys and tokens that still work. */
export function liveKeys(list: ApiKeyView[], now = Date.now()): ApiKeyView[] {
  return list.filter((k) => !k.revoked && (!k.expires_at || Date.parse(k.expires_at) > now));
}

/** Functions with a live version. */
export function deployedFunctions(list: FunctionInfo[]): FunctionInfo[] {
  return list.filter((f) => f.active_version > 0);
}
