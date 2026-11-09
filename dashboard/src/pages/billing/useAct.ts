import { useCallback, useState } from "react";

/** Run a mutation: tracks busy and the last error, then calls `after` (usually a reload) on success. */
export function useAct(after?: () => void) {
  const [error, setError] = useState<unknown>();
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string>();
  const run = useCallback(
    async <T,>(fn: () => Promise<T>, message?: string): Promise<T | undefined> => {
      setBusy(true);
      setError(undefined);
      setNote(undefined);
      try {
        const v = await fn();
        if (message) setNote(message);
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
  return { run, error, busy, note };
}
