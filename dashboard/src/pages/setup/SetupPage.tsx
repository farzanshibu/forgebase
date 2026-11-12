import { useEffect, useState, type FormEvent } from "react";
import { Link, Navigate, useSearchParams } from "react-router-dom";
import { CheckmarkCircle02Icon, InformationCircleIcon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { Alert } from "panelui-web/alert";
import { Input } from "panelui-web/input";
import { ApiError, createSetupAdmin } from "../../api";
import { useSession } from "../../auth/session";
import ThemeMenu from "../../components/ThemeMenu";
import { AuthShell, BrandMark } from "../../components/AuthShell";
import { ErrorPanel } from "../../components/ErrorPanel";
import { FormField } from "../../components/FormField";
import { PasswordInput, PasswordStrength } from "../../components/PasswordInput";
import { Button, Loading } from "../../components/ui";
import FeaturesFlow from "./FeaturesFlow";
import { useFeatureData } from "./features";
import { useSetupStatus } from "./useSetupStatus";

/**
 * `/setup`: create the administrator (step 1) while the server is in setup mode, then the feature map, configure and done steps.
 * Anyone arriving when setup is already complete goes to Overview.
 */
export default function SetupPage() {
  const { status } = useSession();
  const setup = useSetupStatus();
  // True once this page created the administrator: the wizard then carries on instead of leaving for Overview.
  const [created, setCreated] = useState(false);
  const authed = status === "authenticated";
  const features = useFeatureData(authed);

  if (!authed && setup.loading) return <Loading what="Setup" />;
  if (!authed && !setup.required) return <Navigate to="/" replace />;
  if (authed && !created && !setup.loading && !setup.required) return <Navigate to="/" replace />;

  // Step 1 (not signed in yet): the administrator form, in the same frame as sign-in.
  if (!authed) return <CreateAdmin onCreated={() => setCreated(true)} />;

  return (
    <main className="relative min-h-svh bg-background">
      <div aria-hidden="true" className="pointer-events-none absolute inset-x-0 top-0 h-72 bg-[radial-gradient(50rem_18rem_at_50%_-20%,color-mix(in_oklab,var(--primary)_10%,transparent),transparent)]" />
      <div className="relative mx-auto max-w-5xl space-y-8 px-4 py-6 md:px-8 md:py-10">
        <header className="flex items-center gap-3">
          <BrandMark />
          <div className="min-w-0 flex-1">
            <h1 className="text-xl font-semibold tracking-tight">Set up ForgeBase</h1>
            <p className="text-sm text-muted-foreground">Choose the features this server runs, then configure them. You can change all of it later in Features and settings.</p>
          </div>
          <ThemeMenu />
        </header>
        {features.loading ? (
          <Loading what="Features" />
        ) : features.snapshot ? (
          <FeaturesFlow snapshot={features.snapshot} apply={features.apply} before={1} />
        ) : (
          <ErrorPanel error={features.error ?? new Error("The feature list is not available")} />
        )}
      </div>
    </main>
  );
}

const STEPS = ["Administrator", "Features", "Configure", "Done"];

/** Where the wizard is: four bars, the current step's name in bold. Same steps as FeaturesFlow shows after step 1. */
function SetupProgress({ current }: { current: number }) {
  return (
    <div className="space-y-2">
      <p className="text-xs font-medium uppercase tracking-wider text-primary">
        Step {current + 1} of {STEPS.length}
      </p>
      <ol aria-label="Setup steps" className="grid grid-cols-4 gap-2">
        {STEPS.map((s, i) => (
          <li key={s} aria-current={i === current ? "step" : undefined} className="space-y-1.5">
            <span aria-hidden="true" className={`block h-1 rounded-full ${i <= current ? "bg-primary" : "bg-border"}`} />
            <span className={`block truncate text-xs ${i === current ? "font-medium text-foreground" : "text-muted-foreground"}`}>{s}</span>
          </li>
        ))}
      </ol>
    </div>
  );
}

/** Same rule as the server's `ADMIN_PASSWORD_MIN` (crates/forgebase-auth/src/service/machine.rs). */
export const PASSWORD_MIN = 10;

function CreateAdmin({ onCreated }: { onCreated: () => void }) {
  const { adoptSession } = useSession();
  const [params] = useSearchParams();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [confirmTouched, setConfirmTouched] = useState(false);
  const [token, setToken] = useState(params.get("token") ?? "");
  const [busy, setBusy] = useState(false);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [error, setError] = useState<unknown>();
  useEffect(() => {
    // The setup link carries the token; keep it out of the address bar and history once read.
    if (params.get("token")) window.history.replaceState(window.history.state, "", window.location.pathname);
  }, [params]);

  // The confirm field is checked when it loses focus, not only on submit (first-run contract, rule 4).
  const mismatch = confirm !== password ? "The passwords do not match" : undefined;
  const confirmError = (confirmTouched ? mismatch : undefined) ?? errors.confirm;
  const clear = (k: string) => setErrors((e) => (k in e ? Object.fromEntries(Object.entries(e).filter(([x]) => x !== k)) : e));

  async function submit(e: FormEvent) {
    e.preventDefault();
    setError(undefined);
    if (mismatch) {
      setConfirmTouched(true);
      return setErrors({ confirm: mismatch });
    }
    setErrors({});
    setBusy(true);
    try {
      const s = await createSetupAdmin(token.trim(), email.trim(), password);
      onCreated();
      await adoptSession(s);
    } catch (err) {
      if (err instanceof ApiError && err.code === "invalid_setup_token") setErrors({ token: "This setup token is not valid. Use the link printed by the server." });
      else if (err instanceof ApiError && err.code === "validation_failed") {
        const f: Record<string, string> = { ...err.details };
        for (const [k, v] of Object.entries(err.fields)) f[k] = v.message || v.code;
        setErrors(Object.keys(f).length > 0 ? f : {});
        if (Object.keys(f).length === 0) setError(err);
      } else setError(err);
    } finally {
      setBusy(false);
    }
  }

  const fromLink = params.get("token") !== null && token !== "" && !errors.token;
  const tokenField = (
    <FormField
      label="Setup token"
      error={errors.token}
      help={
        fromLink ? (
          <span className="inline-flex items-center gap-1 text-success">
            <HugeiconsIcon icon={CheckmarkCircle02Icon} size={14} strokeWidth={2} aria-hidden="true" />
            Filled in from your setup link.
          </span>
        ) : (
          <>Printed by <code>forgebase serve</code> as the setup link; it fills in when you open that link.</>
        )
      }
    >
      {(p) => <Input {...p} className="w-full font-mono text-xs" value={token} onChange={(e) => { setToken(e.target.value); clear("token"); }} autoComplete="off" spellCheck={false} placeholder="Paste the token from the server log" required />}
    </FormField>
  );

  return (
    <AuthShell
      width="md"
      eyebrow={<SetupProgress current={0} />}
      title="Create the administrator"
      description="This account manages the whole server. It can be created once; add more administrators later from Authentication."
    >
      <form onSubmit={submit} aria-label="Create administrator" className="space-y-5" noValidate>
        {!fromLink && token === "" && (
          <div className="flex gap-2.5 rounded-xl border border-info/25 bg-info/5 p-3 text-xs text-foreground">
            <HugeiconsIcon icon={InformationCircleIcon} size={16} strokeWidth={1.8} className="mt-px shrink-0 text-info" aria-hidden="true" />
            <p>
              When it starts, <code>forgebase serve</code> prints a setup link. Open that link to fill in the token below, or paste
              the token by hand. It proves you can see the server's log.
            </p>
          </div>
        )}
        <FormField label="Email" error={errors.email}>
          {(p) => <Input {...p} className="w-full" type="email" value={email} onChange={(e) => { setEmail(e.target.value); clear("email"); }} autoComplete="username" placeholder="you@example.com" autoFocus required />}
        </FormField>
        <FormField label="Password" help={`At least ${PASSWORD_MIN} characters. A long phrase of several words is easy to remember and hard to guess.`} error={errors.password}>
          {(p) => (
            <div className="space-y-2">
              <PasswordInput {...p} value={password} onChange={(e) => { setPassword(e.target.value); clear("password"); }} autoComplete="new-password" required />
              <PasswordStrength value={password} min={PASSWORD_MIN} />
            </div>
          )}
        </FormField>
        <FormField
          label="Confirm password"
          error={confirmError}
          help={confirm !== "" && !mismatch ? <span className="inline-flex items-center gap-1 text-success"><HugeiconsIcon icon={CheckmarkCircle02Icon} size={14} strokeWidth={2} aria-hidden="true" />The passwords match.</span> : undefined}
        >
          {(p) => (
            <PasswordInput
              {...p} value={confirm} autoComplete="new-password" required
              onChange={(e) => { setConfirm(e.target.value); clear("confirm"); }}
              onBlur={() => confirm !== "" && setConfirmTouched(true)}
            />
          )}
        </FormField>
        {tokenField}
        {error !== undefined &&
          (error instanceof ApiError && error.code === "setup_complete" ? (
            <Alert tone="info" variant="plain">
              <Alert.Icon />
              <Alert.Title>Setup is already complete</Alert.Title>
              <Alert.Description>An administrator exists. <Link className="underline" to="/">Sign in</Link> instead.</Alert.Description>
            </Alert>
          ) : (
            <ErrorPanel error={error} />
          ))}
        <Button type="submit" tone="primary" className="h-10 w-full text-sm" disabled={busy}>
          {busy ? "Creating administrator…" : "Create administrator"}
        </Button>
      </form>
    </AuthShell>
  );
}
