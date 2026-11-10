import { useSearchParams } from "react-router-dom";
import { getSystemInfo } from "./api";
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

export interface LoadState<T> {
  data: T | undefined;
  error: unknown;
  loading: boolean;
  reload: () => void;
}

/** Load once per `key` (and per `reload()`); the previous key's data is never shown for a new key. */
export function useLoad<T>(fn: (signal: AbortSignal) => Promise<T>, key: string): LoadState<T> {
  const [state, setState] = useState<{ key: string; data?: T; error?: unknown }>();
  const [tick, setTick] = useState(0);
  const fnRef = useRef(fn);
  useEffect(() => {
    fnRef.current = fn;
  });
  const full = `${key}#${tick}`;
  useEffect(() => {
    const ctrl = new AbortController();
    fnRef.current(ctrl.signal).then(
      (data) => !ctrl.signal.aborted && setState({ key: full, data }),
      (error: unknown) => !ctrl.signal.aborted && setState({ key: full, error }),
    );
    return () => ctrl.abort();
  }, [full]);
  const reload = useCallback(() => setTick((t) => t + 1), []);
  const fresh = state?.key === full;
  // After reload() keep showing the last data of the same key rather than flashing empty.
  const sameKey = state?.key.split("#")[0] === key;
  return {
    data: fresh || sameKey ? state?.data : undefined,
    error: fresh ? state?.error : undefined,
    loading: !fresh,
    reload,
  };
}

export interface CursorPage<T> {
  items: T[];
  /** Cursor for the next page; absent or null on the last page. */
  next?: string | null;
}

export interface CursorListState<T> {
  items: T[];
  error: unknown;
  /** The first page is loading. */
  loading: boolean;
  /** A later page is loading. */
  loadingMore: boolean;
  hasMore: boolean;
  loadMore: () => void;
  reload: () => void;
}

/**
 * A stream read page by page with "Load more" (logs, objects, messages). Pair it with `<Pager mode="more">`.
 *
 * Usage:
 *   const list = useCursorList((cursor, signal) => listObjects(bucket, { cursor }, signal).then((p) => ({ items: p.items, next: p.next_cursor })), `objects:${bucket}`);
 *   <Pager mode="more" hasMore={list.hasMore} loading={list.loadingMore} onMore={list.loadMore} shown={list.items.length} />
 */
export function useCursorList<T>(fetchPage: (cursor: string | undefined, signal: AbortSignal) => Promise<CursorPage<T>>, key: string): CursorListState<T> {
  const [gen, setGen] = useState(0);
  const full = `${key}#${gen}`;
  const first = useLoad((s) => fetchPage(undefined, s), full);
  const [extra, setExtra] = useState<{ key: string; items: T[]; next: string | null }>();
  const [moreError, setMoreError] = useState<{ key: string; error: unknown }>();
  const [busy, setBusy] = useState(false);
  const fnRef = useRef(fetchPage);
  useEffect(() => {
    fnRef.current = fetchPage;
  });
  const mine = extra?.key === full ? extra : undefined;
  const next = mine ? mine.next : (first.data?.next ?? null);
  const loadMore = useCallback(() => {
    if (!next || busy) return;
    setBusy(true);
    setMoreError(undefined);
    fnRef.current(next, new AbortController().signal).then(
      (p) => {
        setExtra((x) => ({ key: full, items: [...(x?.key === full ? x.items : []), ...p.items], next: p.next ?? null }));
        setBusy(false);
      },
      (error: unknown) => {
        setMoreError({ key: full, error });
        setBusy(false);
      },
    );
  }, [next, busy, full]);
  const reload = useCallback(() => setGen((g) => g + 1), []);
  return {
    items: [...(first.data?.items ?? []), ...(mine?.items ?? [])],
    error: (moreError?.key === full ? moreError.error : undefined) ?? first.error,
    loading: first.loading,
    loadingMore: busy,
    hasMore: !!next,
    loadMore,
    reload,
  };
}

let infoCache: { at: number; version?: string } | undefined;
let infoPending: Promise<string | undefined> | undefined;

/** The running server's version (GET /api/system/info), fetched once per app load and shared. `undefined` until known or when it fails. */
export function useServerVersion(): string | undefined {
  const [version, setVersion] = useState<string | undefined>(infoCache?.version);
  useEffect(() => {
    if (infoCache) return;
    let live = true;
    infoPending ??= getSystemInfo()
      .then((i) => i.version)
      .catch(() => undefined)
      .then((v) => {
        infoCache = { at: Date.now(), version: v };
        return v;
      });
    void infoPending.then((v) => live && setVersion(v));
    return () => {
      live = false;
    };
  }, []);
  return version;
}

/** Test hook: forget the cached server version. */
export function resetServerVersion() {
  infoCache = undefined;
  infoPending = undefined;
}

/**
 * `?new=1` on the current route: the command palette's "Create" actions and links use it to open a page's CreateSheet.
 *
 * Usage:
 *   const [creating, setCreating] = useCreateParam();
 *   <CreateSheet open={creating} onOpenChange={setCreating} … />
 */
export function useCreateParam(): [boolean, (open: boolean) => void] {
  const [params, setParams] = useSearchParams();
  const open = params.get("new") === "1";
  const set = useCallback(
    (v: boolean) =>
      setParams(
        (p) => {
          const n = new URLSearchParams(p);
          if (v) n.set("new", "1");
          else n.delete("new");
          return n;
        },
        { replace: true },
      ),
    [setParams],
  );
  return [open, set];
}
