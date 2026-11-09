import { useState, type FormEvent } from "react";
import { ApiError } from "../api";
import { useSession } from "../auth/session";
import { Button, inputCls } from "../components/ui";

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
    <main className="flex min-h-screen items-center justify-center p-6">
      <form onSubmit={submit} aria-label="Sign in" className="w-full max-w-sm space-y-4 rounded-lg border border-zinc-800 bg-zinc-900 p-6">
        <h1 className="text-lg font-semibold tracking-tight">
          Forge<span className="text-amber-400">Base</span> admin
        </h1>
        {expiredNotice && (
          <p role="status" className="rounded-md border border-amber-700/60 bg-amber-500/10 p-3 text-sm text-amber-300">
            Your session expired. Sign in again.
          </p>
        )}
        {mfaRequired ? (
          <div>
          <label className="block text-sm">
            <span className="text-zinc-300">Authentication code</span>
            <input
              className={`${inputCls} mt-1 w-full`} value={code} onChange={(e) => setCode(e.target.value)}
              autoComplete="one-time-code" inputMode="numeric" autoFocus required
            />
          </label>
            <p className="mt-1 text-xs text-zinc-500">Enter the code from your authenticator app, or a recovery code.</p>
          </div>
        ) : (
          <>
            <label className="block text-sm">
              <span className="text-zinc-300">Email</span>
              <input className={`${inputCls} mt-1 w-full`} type="email" value={email} onChange={(e) => setEmail(e.target.value)} autoComplete="username" autoFocus required />
            </label>
            <label className="block text-sm">
              <span className="text-zinc-300">Password</span>
              <input className={`${inputCls} mt-1 w-full`} type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="current-password" required />
            </label>
          </>
        )}
        {error && (
          <p role="alert" className="rounded-md border border-red-800/60 bg-red-500/10 p-3 text-sm text-red-300">
            {error}
          </p>
        )}
        <Button type="submit" tone="primary" className="w-full" disabled={busy}>
          {mfaRequired ? "Verify" : "Sign in"}
        </Button>
      </form>
    </main>
  );
}
