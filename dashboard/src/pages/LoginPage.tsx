import { useState, type FormEvent } from "react";
import { ApiError } from "../api";
import { useSession } from "../auth/session";
import { Fire03Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { Alert } from "panelui-web/alert";
import { Card } from "panelui-web/card";
import { Input } from "panelui-web/input";
import ThemeMenu from "../components/ThemeMenu";
import { Button } from "../components/ui";

export default function LoginPage() {
  const { signIn, submitMfa, mfaRequired, expiredNotice } = useSession();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [code, setCode] = useState("");
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);

  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(undefined);
    try {
      if (mfaRequired) await submitMfa(code.trim());
      else await signIn(email.trim(), password);
    } catch (err) {
      setError(err instanceof ApiError ? err.message : err instanceof Error ? err.message : String(err));
      setCode("");
    } finally {
      setBusy(false);
    }
  }

  return (
    <main className="relative flex min-h-svh items-center justify-center bg-background p-4">
      <div aria-hidden="true" className="pointer-events-none absolute inset-0 bg-[radial-gradient(60rem_30rem_at_50%_-10%,color-mix(in_oklab,var(--primary)_14%,transparent),transparent)]" />
      <div className="absolute end-4 top-4">
        <ThemeMenu />
      </div>
      <Card className="relative w-full max-w-sm">
        <Card.Header>
          <span className="mb-2 flex size-10 items-center justify-center rounded-xl bg-primary text-primary-foreground" aria-hidden="true">
            <HugeiconsIcon icon={Fire03Icon} size={22} strokeWidth={2} />
          </span>
          <h1 className="text-lg font-semibold leading-tight tracking-tight">ForgeBase admin</h1>
          <Card.Description>{mfaRequired ? "Two-step verification" : "Sign in to manage this server."}</Card.Description>
        </Card.Header>
        <Card.Content>
          <form onSubmit={submit} aria-label="Sign in" className="space-y-4">
            {expiredNotice && (
              <Alert tone="info" variant="plain">
                <Alert.Icon />
                <Alert.Title>Your session expired. Sign in again.</Alert.Title>
              </Alert>
            )}
            {mfaRequired ? (
              <div>
                <label className="block space-y-1.5 text-sm">
                  <span className="font-medium">Authentication code</span>
                  <Input
                    className="w-full" value={code} onChange={(e) => setCode(e.target.value)}
                    autoComplete="one-time-code" inputMode="numeric" autoFocus required
                  />
                </label>
                <p className="mt-1.5 text-xs text-muted-foreground">Enter the code from your authenticator app, or a recovery code.</p>
              </div>
            ) : (
              <>
                <label className="block space-y-1.5 text-sm">
                  <span className="font-medium">Email</span>
                  <Input className="w-full" type="email" value={email} onChange={(e) => setEmail(e.target.value)} autoComplete="username" autoFocus required />
                </label>
                <label className="block space-y-1.5 text-sm">
                  <span className="font-medium">Password</span>
                  <Input className="w-full" type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="current-password" required />
                </label>
              </>
            )}
            {error && (
              <Alert tone="destructive" variant="plain">
                <Alert.Icon />
                <Alert.Title>{error}</Alert.Title>
              </Alert>
            )}
            <Button type="submit" tone="primary" className="w-full" disabled={busy}>
              {mfaRequired ? "Verify" : "Sign in"}
            </Button>
          </form>
        </Card.Content>
      </Card>
    </main>
  );
}
