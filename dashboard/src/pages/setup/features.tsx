import { createContext, useCallback, useContext, useEffect, useState } from "react";
import { getFeatures, type ServerFeature, type FeaturesResponse } from "../../api";

export interface FeatureData {
  snapshot: FeaturesResponse | undefined;
  error: unknown;
  loading: boolean;
  /** Replace the snapshot with a fresher one (a save answers with the full list). */
  apply: (features: ServerFeature[], restartPending?: boolean) => void;
  reload: () => void;
}

/** Loads `GET /api/system/features` while `enabled`. A failure leaves `snapshot` empty so callers can degrade. */
export function useFeatureData(enabled = true): FeatureData {
  const [snapshot, setSnapshot] = useState<FeaturesResponse>();
  const [error, setError] = useState<unknown>();
  const [loading, setLoading] = useState(enabled);
  const [tick, setTick] = useState(0);
  useEffect(() => {
    if (!enabled) return;
    const ctrl = new AbortController();
    getFeatures(ctrl.signal).then(
      (r) => {
        if (ctrl.signal.aborted) return;
        setSnapshot(r);
        setError(undefined);
        setLoading(false);
      },
      (e) => {
        if (ctrl.signal.aborted) return;
        setError(e);
        setLoading(false);
      },
    );
    return () => ctrl.abort();
  }, [enabled, tick]);
  const apply = useCallback((features: ServerFeature[], restartPending?: boolean) => {
    setSnapshot((s) => (s ? { ...s, features, restart_pending: restartPending ?? s.restart_pending } : s));
  }, []);
  const reload = useCallback(() => setTick((t) => t + 1), []);
  return { snapshot, error, loading, apply, reload };
}

export const FeaturesContext = createContext<FeatureData | null>(null);

/** The layout's shared feature list (nav hiding and Features & settings read the same copy). */
export function useSharedFeatures(): FeatureData | null {
  return useContext(FeaturesContext);
}

const FALLBACK_PATHS: Record<string, string> = { storage: "storage", functions: "functions", jobs: "jobs", queues: "queues", vector: "vector", graph: "graph", identity: "identity", access: "access", resp: "redis", billing: "billing" };

/** Studio paths (no leading slash) whose feature is switched off. */
export function disabledStudioPaths(features: ServerFeature[] | undefined): Set<string> {
  const out = new Set<string>();
  for (const f of features ?? []) {
    if (f.enabled || f.always_on) continue;
    const p = (f.studio_path ?? FALLBACK_PATHS[f.id] ?? "").replace(/^\/+/, "");
    if (p) out.add(p);
  }
  return out;
}
