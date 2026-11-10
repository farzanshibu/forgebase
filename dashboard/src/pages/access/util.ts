import { useCallback, useState } from "react";

/** Runs mutations: tracks the busy flag and last error, then calls `after` (usually a reload). */
export function useRun(after?: () => void) {
  const [error, setError] = useState<unknown>();
  const [busy, setBusy] = useState(false);
  const run = useCallback(
    async <T,>(fn: () => Promise<T>): Promise<T | undefined> => {
      setBusy(true);
      setError(undefined);
      try {
        const v = await fn();
        after?.();
        return v;
      } catch (e) {
        setError(e);
        return undefined;
      } finally {
        setBusy(false);
      }
    },
    [after],
  );
  return { run, error, busy, clearError: () => setError(undefined) };
}

export const rowCls = (deny?: boolean) => `border-t border-border ${deny ? "bg-destructive/10" : ""}`;
