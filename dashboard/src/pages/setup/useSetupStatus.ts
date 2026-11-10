import { useEffect, useState } from "react";
import { getSetupStatus } from "../../api";

/** `GET /api/setup/status` once. A failed probe counts as "not required" so sign-in is never blocked by it. */
export function useSetupStatus(enabled = true): { loading: boolean; required: boolean } {
  const [state, setState] = useState<{ loading: boolean; required: boolean }>({ loading: enabled, required: false });
  useEffect(() => {
    if (!enabled) return;
    const ctrl = new AbortController();
    getSetupStatus(ctrl.signal).then(
      (r) => !ctrl.signal.aborted && setState({ loading: false, required: r.setup_required === true }),
      () => !ctrl.signal.aborted && setState({ loading: false, required: false }),
    );
    return () => ctrl.abort();
  }, [enabled]);
  return state;
}
