import { useState } from "react";
import { useLoad } from "../../hooks";
import type { Page } from "../../api";

/** First page through `useLoad` (re-runs when `key` changes or `reload()` is called); later pages are appended by `more()`. */
export function usePaged<T>(fetchPage: (after: string | undefined, signal?: AbortSignal) => Promise<Page<T>>, key: string) {
  const [gen, setGen] = useState(0);
  const full = `${key}#${gen}`;
  const first = useLoad((s) => fetchPage(undefined, s), full);
  const [extra, setExtra] = useState<{ key: string; items: T[]; next: string | null }>();
  const [error, setError] = useState<unknown>();
  const mine = extra?.key === full ? extra : undefined;
  const items = [...(first.data?.items ?? []), ...(mine?.items ?? [])];
  const next = mine ? mine.next : (first.data?.next_after ?? null);

  async function more() {
    if (!next) return;
    setError(undefined);
    try {
      const p = await fetchPage(next);
      setExtra({ key: full, items: [...(mine?.items ?? []), ...p.items], next: p.next_after ?? null });
    } catch (e) {
      setError(e);
    }
  }
  return { items, next, more, loading: first.loading, loaded: first.data !== undefined, error: error ?? first.error, reload: () => setGen((g) => g + 1), setError };
}
