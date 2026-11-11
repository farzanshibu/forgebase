/**
 * ErrorPanel: an error in plain words, what to do next, a copyable request id, and the raw error behind Details
 * (dashboard-ux.md rule 4, audit A-04). `ErrorView` in ui.tsx is this component, so every screen already uses it.
 *
 * Usage:
 *   <ErrorPanel error={load.error} />                         // renders nothing when error is falsy
 *   <ErrorPanel error={saveError} fields="hide" />            // the form shows field errors next to its fields
 *   <ErrorPanel error={e} onRetry={reload} />                 // adds a "Try again" button
 *   const byField = fieldErrors(saveError);                   // re-exported from errors.ts for FormField
 *
 * With a request id the panel also links "Find in logs" (Functions › Logs search, request id prefilled) when it is
 * inside a router.
 *
 * Wording comes from src/errors.ts. 401 says the session ended; 403 names the missing permission and never says
 * "sign in". A feature switched off (feature_disabled) shows where to switch it back on.
 */
import { Alert } from "panelui-web/alert";
import { useState } from "react";
import { Link, useInRouterContext } from "react-router-dom";
import { explain, fieldErrors } from "../errors";
import { ApiError } from "../api";
import { Button, CopyButton } from "./Button";

// Forms import fieldErrors with ErrorPanel (one import for the error UI); it is the same function as in errors.ts.
// eslint-disable-next-line react-refresh/only-export-components
export { fieldErrors };

export interface ErrorPanelProps {
  error: unknown;
  /** "list" (default) lists per-field errors under the message; "hide" when a form shows them next to its fields. */
  fields?: "list" | "hide";
  /** Shows a "Try again" button. */
  onRetry?: () => void;
  className?: string;
}

export function ErrorPanel({ error, fields = "list", onRetry, className }: ErrorPanelProps) {
  const inRouter = useInRouterContext();
  if (!error) return null;
  if (error instanceof ApiError && error.code === "feature_disabled") return <FeatureDisabled message={error.message} />;
  const e = explain(error);
  const fromServer = error instanceof ApiError;
  const byField = fields === "list" ? Object.entries(fieldErrors(error)) : [];
  const tone = e.kind === "session" || e.kind === "forbidden" ? "warning" : "destructive";
  // The server's own words, when the title is our plain version of them.
  const serverSaid = fromServer && e.message && e.message !== e.title ? e.message : undefined;
  return (
    <Alert tone={tone} variant="plain" className={className}>
      <Alert.Icon />
      <Alert.Title>{e.title}</Alert.Title>
      {fromServer ? (
        <Alert.Description>
          <p>
            <span className="font-medium text-foreground">What to do: </span>
            {e.next}
          </p>
          {byField.length > 0 && (
            <ul aria-label="Fields to fix" className="mt-1 list-disc space-y-0.5 pl-5">
              {byField.map(([name, msg]) => (
                <li key={name}>
                  <span className="font-medium text-foreground">{name}</span>: {msg}
                </li>
              ))}
            </ul>
          )}
          {serverSaid && <p className="mt-1 text-xs">Server message: {serverSaid}</p>}
          <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-2 text-xs">
            {onRetry && (
              <Button type="button" onClick={onRetry}>
                Try again
              </Button>
            )}
            {e.code && <span className="font-mono">Code {e.code}</span>}
            {e.requestId && (
              <>
                <span className="font-mono">Request id {e.requestId}</span>
                <CopyButton text={e.requestId} label="Copy request id" />
                {inRouter && (
                  <Link to={`/functions/logs?request=${encodeURIComponent(e.requestId)}`} className="font-medium text-primary underline-offset-2 hover:underline">
                    Find in logs
                  </Link>
                )}
              </>
            )}
          </div>
          <RawDetails status={e.status} raw={e.raw} />
        </Alert.Description>
      ) : (
        onRetry && (
          <Alert.Description>
            <Button type="button" onClick={onRetry} className="mt-1">
              Try again
            </Button>
          </Alert.Description>
        )
      )}
    </Alert>
  );
}

/** "Details": the status and the raw response, rendered only once opened so it never repeats the message on screen. */
function RawDetails({ status, raw }: { status?: number; raw: unknown }) {
  const [open, setOpen] = useState(false);
  return (
    <details className="mt-2 text-xs" onToggle={(ev) => setOpen((ev.currentTarget as HTMLDetailsElement).open)}>
      <summary className="cursor-pointer select-none font-medium text-foreground">Details</summary>
      {open && (
        <div className="mt-1 space-y-1">
          {status !== undefined && <p>HTTP status {status}</p>}
          <pre className="max-h-60 overflow-auto rounded-md bg-muted/60 p-2 font-mono text-foreground/80">{stringify(raw)}</pre>
        </div>
      )}
    </details>
  );
}

function stringify(v: unknown): string {
  try {
    return JSON.stringify(v, null, 2) ?? String(v);
  } catch {
    return String(v);
  }
}

/** A module the administrator switched off: say so, and point at where to switch it back on. */
export function FeatureDisabled({ message }: { message: string }) {
  const inRouter = useInRouterContext();
  return (
    <section aria-label="Feature disabled" className="max-w-xl rounded-lg border border-dashed border-border p-6">
      <p className="font-medium">{message}</p>
      <p className="mt-1 text-sm text-muted-foreground">An administrator turned this feature off, so the server does not run it.</p>
      {inRouter ? (
        <Link to="/system/features" className="mt-3 inline-block text-sm font-medium text-primary underline-offset-2 hover:underline">
          Open Features and settings
        </Link>
      ) : null}
    </section>
  );
}

export default ErrorPanel;
