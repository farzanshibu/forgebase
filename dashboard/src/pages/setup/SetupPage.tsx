import { useEffect, useState, type FormEvent } from "react";
import { Link, Navigate, useSearchParams } from "react-router-dom";
import { Fire03Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { Alert } from "panelui-web/alert";
import { Card } from "panelui-web/card";
import { Input } from "panelui-web/input";
import { ApiError, createSetupAdmin } from "../../api";
import { useSession } from "../../auth/session";
import ThemeMenu from "../../components/ThemeMenu";
import { Button, ErrorView, Loading } from "../../components/ui";
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

  return (
    <main className="relative min-h-svh bg-background p-4 md:p-8">
      <div className="absolute end-4 top-4">
        <ThemeMenu />
      </div>
      <div className="mx-auto max-w-5xl space-y-6">
        <header className="flex items-center gap-3">
          <span className="flex size-10 items-center justify-center rounded-xl bg-primary text-primary-foreground" aria-hidden="true">
            <HugeiconsIcon icon={Fire03Icon} size={22} strokeWidth={2} />
          </span>
          <div>
            <h1 className="text-xl font-semibold tracking-tight">Set up ForgeBase</h1>
            <p className="text-sm text-muted-foreground">First run: create the administrator, choose features, then configure them.</p>
          </div>
        </header>
        {!authed ? (
          <CreateAdmin onCreated={() => setCreated(true)} />
        ) : features.loading ? (
          <Loading what="Features" />
        ) : features.snapshot ? (
          <FeaturesFlow snapshot={features.snapshot} apply={features.apply} before={1} />
        ) : (
          <ErrorView error={features.error ?? new Error("The feature list is not available")} />
        )}
      </div>
    </main>
  );
}

function CreateAdmin({ onCreated }: { onCreated: () => void }) {
  const { adoptSession } = useSession();
  const [params] = useSearchParams();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [token, setToken] = useState(params.get("token") ?? "");
  const [busy, setBusy] = useState(false);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [error, setError] = useState<unknown>();
  useEffect(() => {
    // The setup link carries the token; keep it out of the address bar and history once read.
    if (params.get("token")) window.history.replaceState(window.history.state, "", window.location.pathname);
  }, [params]);

  async function submit(e: FormEvent) {
    e.preventDefault();
    setError(undefined);
    if (password !== confirm) return setErrors({ confirm: "The passwords do not match" });
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

  const fieldErr = (k: string) => errors[k] && (
    <p role="alert" className="text-xs font-medium text-destructive">
      {errors[k]}
    </p>
  );
  return (
    <Card className="max-w-md" aria-label="Create administrator">
      <Card.Header>
        <h2 className="text-lg font-semibold tracking-tight">Create the administrator</h2>
        <Card.Description>This account manages the server. It can be created once.</Card.Description>
      </Card.Header>
      <Card.Content>
        <form onSubmit={submit} aria-label="Create administrator" className="space-y-4">
          <label className="block space-y-1.5 text-sm">
            <span className="font-medium">Email</span>
            <Input className="w-full" type="email" value={email} onChange={(e) => setEmail(e.target.value)} autoComplete="username" required />
            {fieldErr("email")}
          </label>
          <label className="block space-y-1.5 text-sm">
            <span className="font-medium">Password</span>
            <Input className="w-full" type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="new-password" required />
            {fieldErr("password")}
          </label>
          <label className="block space-y-1.5 text-sm">
            <span className="font-medium">Confirm password</span>
            <Input className="w-full" type="password" value={confirm} onChange={(e) => setConfirm(e.target.value)} autoComplete="new-password" required />
            {fieldErr("confirm")}
          </label>
          <label className="block space-y-1.5 text-sm">
            <span className="font-medium">Setup token</span>
            <Input className="w-full font-mono" value={token} onChange={(e) => setToken(e.target.value)} autoComplete="off" spellCheck={false} required />
            <span className="block text-xs text-muted-foreground">Printed by <code>forgebase serve</code> as the setup link; it fills in when you open that link.</span>
            {fieldErr("token")}
          </label>
          {error !== undefined &&
            (error instanceof ApiError && error.code === "setup_complete" ? (
              <Alert tone="info" variant="plain">
                <Alert.Icon />
                <Alert.Title>Setup is already complete</Alert.Title>
                <Alert.Description>An administrator exists. <Link className="underline" to="/">Sign in</Link> instead.</Alert.Description>
              </Alert>
            ) : (
              <ErrorView error={error} />
            ))}
          <Button type="submit" tone="primary" className="w-full" disabled={busy}>
            Create administrator
          </Button>
        </form>
      </Card.Content>
    </Card>
  );
}
