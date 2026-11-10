/**
 * Sign-in for administrators. While the server has no administrator, App sends `/` to the first-run setup instead
 * (FB-92), so this page only shows on a server that has one. Errors use ErrorPanel's plain words; a wrong password
 * reads "The email or password is wrong", not a raw code.
 */
import { useState, type FormEvent } from "react";
import { useSession } from "../auth/session";
import { Fire03Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { Alert } from "panelui-web/alert";
import { Card } from "panelui-web/card";
import { Input } from "panelui-web/input";
import ThemeMenu from "../components/ThemeMenu";
import { ErrorPanel } from "../components/ErrorPanel";
import { FormField } from "../components/FormField";
import { Button } from "../components/ui";

export default function LoginPage() {
  const { signIn, submitMfa, mfaRequired, expiredNotice } = useSession();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [code, setCode] = useState("");
  const [error, setError] = useState<unknown>();
  const [busy, setBusy] = useState(false);

  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(undefined);
    try {
      if (mfaRequired) await submitMfa(code.trim());
      else await signIn(email.trim(), password);
    } catch (err) {
      setError(err);
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
      <div className="relative w-full max-w-sm space-y-4">
        <Card>
          <Card.Header>
            <span className="mb-2 flex size-10 items-center justify-center rounded-xl bg-primary text-primary-foreground" aria-hidden="true">
              <HugeiconsIcon icon={Fire03Icon} size={22} strokeWidth={2} />
            </span>
            <h1 className="text-lg font-semibold leading-tight tracking-tight">ForgeBase admin</h1>
            <Card.Description>{mfaRequired ? "Two-step verification: one more code to finish signing in." : "Sign in to manage this server."}</Card.Description>
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
                <FormField label="Authentication code" help="Enter the code from your authenticator app, or a recovery code.">
                  {(p) => (
                    <Input {...p} className="w-full" value={code} onChange={(e) => setCode(e.target.value)} autoComplete="one-time-code" inputMode="numeric" autoFocus required />
                  )}
                </FormField>
              ) : (
                <>
                  <FormField label="Email">
                    {(p) => <Input {...p} className="w-full" type="email" value={email} onChange={(e) => setEmail(e.target.value)} autoComplete="username" autoFocus required />}
                  </FormField>
                  <FormField label="Password">
                    {(p) => <Input {...p} className="w-full" type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="current-password" required />}
                  </FormField>
                </>
              )}
              <ErrorPanel error={error} />
              <Button type="submit" tone="primary" className="w-full" disabled={busy}>
                {busy ? (mfaRequired ? "Verifying…" : "Signing in…") : mfaRequired ? "Verify" : "Sign in"}
              </Button>
            </form>
          </Card.Content>
        </Card>
        {!mfaRequired && (
          <p className="px-1 text-xs leading-relaxed text-muted-foreground">
            Set this server up from the command line, or forgot the password? On the server, run{" "}
            <code className="font-mono text-foreground">forgebase admin list</code> to see the accounts, or{" "}
            <code className="font-mono text-foreground">forgebase admin reset-password --email you@example.com</code> to set a new password.
          </p>
        )}
      </div>
    </main>
  );
}
