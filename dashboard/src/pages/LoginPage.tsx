/**
 * Sign-in for administrators, in two kinds (FB-137): an instance administrator manages the whole server and signs in
 * with the administrator account made at setup; a realm administrator manages one realm and signs in as a user of that
 * realm (they hold identity:admin there and are not instance administrators), with the realm name.
 * The page says which is which, so a realm administrator does not try the instance form and read "wrong password".
 * While the server has no administrator, App sends `/` to the first-run setup instead
 * (FB-92), so this page only shows on a server that has one. Errors use ErrorPanel's plain words; a wrong password
 * reads "The email or password is wrong", not a raw code.
 */
import { useState, type FormEvent } from "react";
import { useSession } from "../auth/session";
import { Building03Icon, ServerStack01Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { Alert } from "panelui-web/alert";
import { Input } from "panelui-web/input";
import { AuthShell } from "../components/AuthShell";
import { ErrorPanel } from "../components/ErrorPanel";
import { FormField } from "../components/FormField";
import { PasswordInput } from "../components/PasswordInput";
import { Button, CopyButton } from "../components/ui";

const KINDS = [
  { id: "admin", label: "Instance administrator", short: "Instance admin", icon: ServerStack01Icon, help: "Manages the whole server. Use the administrator account made when the server was set up." },
  { id: "user", label: "Realm administrator", short: "Realm admin", icon: Building03Icon, help: "Manages one realm. Sign in with your normal user account in that realm: you do not need an instance administrator account." },
] as const;

const RECOVERY = [
  { what: "List the administrator accounts", cmd: "forgebase admin list" },
  { what: "Set a new password", cmd: "forgebase admin reset-password --email you@example.com" },
];

export default function LoginPage() {
  const { signIn, submitMfa, mfaRequired, expiredNotice } = useSession();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [code, setCode] = useState("");
  const [as, setAs] = useState<"admin" | "user">("admin");
  const [realm, setRealm] = useState("");
  const [error, setError] = useState<unknown>();
  const [busy, setBusy] = useState(false);

  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(undefined);
    try {
      if (mfaRequired) await submitMfa(code.trim());
      else await signIn(email.trim(), password, { as, realm });
    } catch (err) {
      setError(err);
      setCode("");
    } finally {
      setBusy(false);
    }
  }

  return (
    <AuthShell
      title={mfaRequired ? "Two-step verification" : "Sign in to ForgeBase"}
      description={mfaRequired ? "One more code to finish signing in." : "Manage this server's data, users and services."}
      footer={!mfaRequired && as === "admin" && <ForgotPassword />}
    >
      <form onSubmit={submit} aria-label="Sign in" className="space-y-5">
        {expiredNotice && (
          <Alert tone="info" variant="plain">
            <Alert.Icon />
            <Alert.Title>Your session expired. Sign in again.</Alert.Title>
          </Alert>
        )}
        {mfaRequired ? (
          <FormField label="Authentication code" help="Enter the code from your authenticator app, or a recovery code.">
            {(p) => (
              <Input
                {...p}
                className="w-full text-center font-mono text-lg tracking-[0.3em]"
                value={code}
                onChange={(e) => setCode(e.target.value)}
                autoComplete="one-time-code"
                inputMode="numeric"
                autoFocus
                required
              />
            )}
          </FormField>
        ) : (
          <>
            <fieldset className="space-y-2">
              <legend className="mb-2 text-sm font-medium">Sign in as</legend>
              {/* A segmented control: two radios drawn as one switch, the chosen kind's explanation under it. */}
              <div className="grid grid-cols-2 gap-1 rounded-xl border border-border bg-muted/60 p-1">
                {KINDS.map((k) => (
                  <label
                    key={k.id}
                    className="relative flex cursor-pointer items-center justify-center gap-1.5 whitespace-nowrap rounded-lg px-2 py-2 text-sm font-medium text-muted-foreground transition-colors hover:text-foreground has-[:checked]:bg-card has-[:checked]:text-foreground has-[:checked]:shadow-sm has-[:checked]:ring-1 has-[:checked]:ring-border has-[:focus-visible]:ring-2 has-[:focus-visible]:ring-ring"
                  >
                    <input
                      type="radio"
                      name="sign-in-kind"
                      className="sr-only"
                      checked={as === k.id}
                      aria-label={k.label}
                      aria-describedby={`sign-in-kind-${k.id}`}
                      onChange={() => {
                        setAs(k.id);
                        setError(undefined);
                      }}
                    />
                    <HugeiconsIcon icon={k.icon} size={16} strokeWidth={1.8} aria-hidden="true" />
                    {/* Short on screen; the radio's name is the full kind (it contains the visible words, WCAG 2.5.3). */}
                    <span aria-hidden="true">{k.short}</span>
                  </label>
                ))}
              </div>
              {KINDS.map((k) => (
                <p key={k.id} id={`sign-in-kind-${k.id}`} hidden={as !== k.id} className="text-xs leading-relaxed text-muted-foreground">
                  {k.help}
                </p>
              ))}
            </fieldset>
            {as === "user" && (
              <FormField label="Realm" help="The realm you manage. Leave it empty for the default realm.">
                {(p) => <Input {...p} className="w-full" value={realm} onChange={(e) => setRealm(e.target.value)} autoComplete="organization" placeholder="default" />}
              </FormField>
            )}
            <FormField label="Email">
              {(p) => (
                <Input {...p} className="w-full" type="email" value={email} onChange={(e) => setEmail(e.target.value)} autoComplete="username" placeholder="you@example.com" autoFocus required />
              )}
            </FormField>
            <FormField label="Password">
              {(p) => <PasswordInput {...p} value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="current-password" required />}
            </FormField>
          </>
        )}
        <ErrorPanel error={error} />
        <Button type="submit" tone="primary" className="h-10 w-full text-sm" disabled={busy}>
          {busy ? (mfaRequired ? "Verifying…" : "Signing in…") : mfaRequired ? "Verify" : "Sign in"}
        </Button>
      </form>
    </AuthShell>
  );
}

/** Instance administrators reset a lost password on the server: the two commands, copyable, behind a disclosure. */
function ForgotPassword() {
  return (
    <details className="group rounded-xl border border-border bg-card/60 text-sm open:bg-card">
      <summary className="flex cursor-pointer list-none items-center justify-between gap-2 rounded-xl px-4 py-3 font-medium text-muted-foreground outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring [&::-webkit-details-marker]:hidden">
        Forgot your password?
        <span className="text-xs transition-transform group-open:rotate-180" aria-hidden="true">
          ▾
        </span>
      </summary>
      <div className="space-y-3 border-t border-border px-4 py-3">
        <p className="text-xs text-muted-foreground">An instance administrator resets it on the server, from a terminal:</p>
        {RECOVERY.map((r) => (
          <div key={r.cmd} className="space-y-1">
            <p className="text-xs font-medium">{r.what}</p>
            <div className="flex items-center gap-2 rounded-lg bg-muted px-2.5 py-1.5">
              <code className="min-w-0 flex-1 truncate text-xs" title={r.cmd}>
                {r.cmd}
              </code>
              <CopyButton text={r.cmd} label="Copy" className="h-7 shrink-0 px-2 text-xs" />
            </div>
          </div>
        ))}
      </div>
    </details>
  );
}
