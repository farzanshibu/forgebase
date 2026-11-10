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
 * Wording comes from src/errors.ts. 401 says the session ended; 403 names the missing permission and never says
 * "sign in". A feature switched off (feature_disabled) shows where to switch it back on.
 */
import { Alert } from "panelui-web/alert";
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
  if (!error) return null;
  if (error instanceof ApiError && error.code === "feature_disabled") return <FeatureDisabled message={error.message} />;
  const e = explain(error);
  const byField = fields === "list" ? Object.entries(fieldErrors(error)) : [];
  const tone = e.kind === "session" || e.kind === "forbidden" ? "warning" : "destructive";
  return (
    <Alert tone={tone} variant="plain" className={className}>
      <Alert.Icon />
      <Alert.Title>{e.title}</Alert.Title>
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
        {(e.requestId || onRetry) && (
          <div className="mt-2 flex flex-wrap items-center gap-2">
            {onRetry && (
              <Button type="button" onClick={onRetry}>
                Try again
              </Button>
            )}
            {e.requestId && (
              <>
                <span className="font-mono text-xs">Request id {e.requestId}</span>
                <CopyButton text={e.requestId} label="Copy request id" />
              </>
            )}
          </div>
        )}
        <details className="mt-2 text-xs">
          <summary className="cursor-pointer select-none font-medium text-foreground">Details</summary>
          <dl className="mt-1 grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5">
            {e.status !== undefined && (
              <>
                <dt>Status</dt>
                <dd className="font-mono">{e.status}</dd>
              </>
            )}
            {e.code && (
              <>
                <dt>Code</dt>
                <dd className="font-mono">{e.code}</dd>
              </>
            )}
            <dt>Message</dt>
            <dd className="break-words">{e.message}</dd>
          </dl>
          <pre className="mt-1 max-h-60 overflow-auto rounded-md bg-muted/60 p-2 font-mono text-foreground/80">{stringify(e.raw)}</pre>
        </details>
      </Alert.Description>
    </Alert>
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
          Open Features &amp; settings
        </Link>
      ) : null}
    </section>
  );
}

export default ErrorPanel;
