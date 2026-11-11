/** Live data for the Database topology screen (FB-176). */
import { useEffect, useRef } from "react";
import { authHeaders, getTopology, listOperations, type TopologyOperation, type TopologyView } from "../../../api";
import { usePolling, type PollState } from "../../../hooks";
import { isFinished } from "../../../api";

/** The topology, polled every 5 s while the tab is visible (a hidden tab keeps its last answer and asks again when shown). */
export function useTopology(): PollState<TopologyView> {
  const last = useRef<TopologyView | undefined>(undefined);
  return usePolling(async (signal) => {
    if (typeof document !== "undefined" && document.hidden && last.current) return last.current;
    const v = await getTopology(signal);
    last.current = v;
    return v;
  }, 5000);
}

/** Operations, newest first: every 2 s while one is running (5 s otherwise). */
export function useOperations(running: boolean): PollState<TopologyOperation[]> {
  return usePolling((signal) => listOperations(false, signal), running ? 2000 : 5000);
}

/**
 * Follow the realtime channel `jobs:<id>` of every running operation (ADR-0023 §3): any message on it means the
 * operation changed, so `onChange` refreshes it from the job (the channel is not replayable, the job is the truth).
 * Without EventSource (old browser, tests) the 2 s poll of `useOperations` does the same job.
 */
export function useJobChannels(ids: string[], onChange: () => void): void {
  const key = ids.slice().sort().join(",");
  const cb = useRef(onChange);
  useEffect(() => {
    cb.current = onChange;
  });
  useEffect(() => {
    if (!key || typeof EventSource === "undefined") return;
    let source: EventSource | undefined;
    let closed = false;
    void authHeaders().then((h) => {
      if (closed) return;
      const subs = key.split(",").map((id) => ({ id: `op-${id}`.slice(0, 64), channel: `jobs:${id}` }));
      const bearer = h.Authorization?.replace(/^Bearer /, "");
      const q = new URLSearchParams({ subscriptions: JSON.stringify(subs), ...(bearer ? { access_token: bearer } : {}) });
      source = new EventSource(`/api/v1/realtime/sse?${q.toString()}`);
      source.onmessage = () => cb.current();
      for (const t of ["message", "publish", "event", "job", "progress"]) source.addEventListener(t, () => cb.current());
      source.onerror = () => undefined; // EventSource reconnects by itself; the poll keeps the list fresh meanwhile
    });
    return () => {
      closed = true;
      source?.close();
    };
  }, [key]);
}

/** Ids of operations that are not finished. */
export const activeIds = (ops: TopologyOperation[] | undefined) => (ops ?? []).filter((o) => !isFinished(o)).map((o) => o.id);
