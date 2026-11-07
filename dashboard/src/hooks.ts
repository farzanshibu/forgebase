import { useCallback, useEffect, useRef, useState } from "react";

export interface PollState<T> {
  data: T | undefined;
  error: unknown;
  loading: boolean;
  updatedAt: Date | undefined;
  refresh: () => void;
}

/** Run `fn` now and every `intervalMs`; keeps the last good `data` when a refresh fails. */
export function usePolling<T>(fn: (signal: AbortSignal) => Promise<T>, intervalMs: number): PollState<T> {
  const [data, setData] = useState<T>();
  const [error, setError] = useState<unknown>();
  const [loading, setLoading] = useState(true);
  const [updatedAt, setUpdatedAt] = useState<Date>();
  const [tick, setTick] = useState(0);
  const fnRef = useRef(fn);
  useEffect(() => {
    fnRef.current = fn;
  });

  useEffect(() => {
    const ctrl = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const run = async () => {
      try {
        const v = await fnRef.current(ctrl.signal);
        if (ctrl.signal.aborted) return;
        setData(v);
        setError(undefined);
        setUpdatedAt(new Date());
      } catch (e) {
        if (ctrl.signal.aborted) return;
        setError(e);
      }
      if (ctrl.signal.aborted) return;
      setLoading(false);
      timer = setTimeout(run, intervalMs);
    };
    void run();
    return () => {
      ctrl.abort();
      if (timer) clearTimeout(timer);
    };
  }, [intervalMs, tick]);

  const refresh = useCallback(() => setTick((t) => t + 1), []);
  return { data, error, loading, updatedAt, refresh };
}
