import { useEffect, useState } from "react";
import { Alert } from "panelui-web/alert";
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
    <Alert tone="destructive" variant="banner" aria-label="Impersonation active" className="bg-destructive/10">
      <Alert.Icon />
      <Alert.Title>Impersonating {impersonation.target}</Alert.Title>
      <Alert.Description>
        Reason: {impersonation.reason} · Time left: <span className="font-mono text-foreground">{left(impersonation.expiresAt, now)}</span>
      </Alert.Description>
      <Alert.Actions>
        <Button
          tone="danger"
          disabled={busy}
          onClick={() => {
            setBusy(true);
            void endImpersonation().finally(() => setBusy(false));
          }}
        >
          End impersonation
        </Button>
      </Alert.Actions>
    </Alert>
  );
}
