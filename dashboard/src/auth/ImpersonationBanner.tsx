import { useEffect, useState } from "react";
import { Button } from "../components/ui";
import { useSession } from "./session";

function left(expiresAt: string, now: number): string {
  const s = Math.max(0, Math.ceil((new Date(expiresAt).getTime() - now) / 1000));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

/** Non-dismissable banner shown above everything while an impersonation is active. */
export default function ImpersonationBanner() {
  const { impersonation, endImpersonation } = useSession();
  const [now, setNow] = useState(() => Date.now());
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    if (!impersonation) return;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [impersonation]);
  if (!impersonation) return null;
  return (
    <div role="alert" aria-label="Impersonation active" className="flex flex-wrap items-center gap-x-4 gap-y-1 border-b border-red-700 bg-red-900 px-4 py-2 text-sm text-red-50">
      <strong>Impersonating {impersonation.target}</strong>
      <span>Reason: {impersonation.reason}</span>
      <span>
        Time left: <span className="font-mono">{left(impersonation.expiresAt, now)}</span>
      </span>
      <Button
        tone="danger"
        className="ml-auto bg-red-950"
        disabled={busy}
        onClick={() => {
          setBusy(true);
          void endImpersonation().finally(() => setBusy(false));
        }}
      >
        End impersonation
      </Button>
    </div>
  );
}
