import type { ButtonHTMLAttributes, ReactNode } from "react";
import { useState } from "react";
import { Alert } from "panelui-web/alert";
import { Button as PanelButton } from "panelui-web/button";
import { Card } from "panelui-web/card";
import { Spinner } from "panelui-web/spinner";
import { ApiError } from "../api";

/** The dashboard's button: PanelUI Button, with the three tones the studios use. */
export function Button({ tone = "default", className = "", ...p }: ButtonHTMLAttributes<HTMLButtonElement> & { tone?: "default" | "primary" | "danger" }) {
  // Danger stays an outline in the destructive colour: a filled destructive button needs white text,
  // which fails AA on the light red the dark theme uses.
  const danger = tone === "danger" ? "border-destructive/50 text-destructive hover:not-disabled:bg-destructive/10 hover:not-disabled:text-destructive" : "";
  return <PanelButton size="sm" variant={tone === "primary" ? "default" : "outline"} {...p} className={`${danger} ${className}`.trim()} />;
}

/** Two-step destructive button: the first click arms it, the second runs `onConfirm`. */
export function ConfirmButton({ label, onConfirm, disabled }: { label: string; onConfirm: () => void; disabled?: boolean }) {
  const [armed, setArmed] = useState(false);
  return armed ? (
    <Button
      tone="danger"
      className="bg-destructive/10"
      onClick={() => {
        setArmed(false);
        onConfirm();
      }}
      onBlur={() => setArmed(false)}
      autoFocus
    >
      Confirm {label.toLowerCase()}
    </Button>
  ) : (
    <Button tone="danger" disabled={disabled} onClick={() => setArmed(true)}>
      {label}
    </Button>
  );
}

/** Classes for native `<select>` and `<textarea>` controls, matching PanelUI Input. */
export const inputCls =
  "rounded-lg border border-input bg-background px-2.5 py-1.5 text-sm text-foreground shadow-xs outline-none transition-[border-color,box-shadow] placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/30 disabled:cursor-not-allowed disabled:opacity-50 aria-invalid:border-destructive dark:bg-input/30";

/** Sign-in-required state for 401/403, otherwise the PLAN section 5 envelope with per-field errors. */
export function ErrorView({ error }: { error: unknown }) {
  if (!error) return null;
  if (error instanceof ApiError && error.isAuth) {
    return (
      <Alert tone="warning" variant="plain">
        <Alert.Icon />
        <Alert.Title>Sign-in required</Alert.Title>
        <Alert.Description>
          The server refused this request ({error.status} {error.code}). Your account may lack the permission, or the session ended:
          sign in again or ask an administrator.
          {error.requestId && <span className="mt-2 block font-mono text-xs">request {error.requestId}</span>}
        </Alert.Description>
      </Alert>
    );
  }
  if (error instanceof ApiError) {
    const fields = Object.entries(error.fields);
    return (
      <Alert tone="destructive" variant="plain">
        <Alert.Icon />
        <Alert.Title>
          <span className="font-mono">{error.code}</span>: {error.message}
        </Alert.Title>
        {(fields.length > 0 || error.requestId) && (
          <Alert.Description>
            {fields.length > 0 && (
              <ul className="mt-1 list-disc space-y-0.5 pl-5">
                {fields.map(([name, f]) => (
                  <li key={name}>
                    <span className="font-mono text-foreground">{name}</span>: {f.message || f.code} <span>({f.code})</span>
                  </li>
                ))}
              </ul>
            )}
            {error.requestId && <span className="mt-2 block font-mono text-xs">request {error.requestId}</span>}
          </Alert.Description>
        )}
      </Alert>
    );
  }
  return (
    <Alert tone="destructive" variant="plain">
      <Alert.Icon />
      <Alert.Title>{error instanceof Error ? error.message : String(error)}</Alert.Title>
    </Alert>
  );
}

export function Loading({ what = "Loading" }: { what?: string }) {
  return (
    <p className="flex items-center gap-2 text-sm text-muted-foreground">
      <Spinner size="sm" aria-hidden="true" role="presentation" />
      {what}…
    </p>
  );
}

export function Panel({ title, actions, children }: { title?: string; actions?: ReactNode; children: ReactNode }) {
  return (
    <Card as="section" size="sm" className="rounded-xl">
      {(title || actions) && (
        <Card.Header className="pb-3">
          {title && (
            <Card.Title as="h2" className="text-sm font-medium text-foreground">
              {title}
            </Card.Title>
          )}
          {actions && <Card.Action className="flex flex-wrap items-center gap-2">{actions}</Card.Action>}
        </Card.Header>
      )}
      <Card.Content>{children}</Card.Content>
    </Card>
  );
}

export function PlanView({ plan }: { plan: { steps: { id: number; parent: number; detail: string }[]; indexes_used: string[]; full_scans: string[] } }) {
  return (
    <div aria-label="Query plan" className="space-y-2 text-sm">
      <ul className="rounded-lg bg-muted/60 p-3 font-mono text-xs text-foreground/80">
        {plan.steps.map((s) => (
          <li key={s.id}>{s.detail}</li>
        ))}
      </ul>
      <p className="text-muted-foreground">
        Indexes used: {plan.indexes_used.length ? plan.indexes_used.join(", ") : "none"}
      </p>
      {plan.full_scans.length > 0 && <p className="text-warning">Full scans: {plan.full_scans.join(", ")}</p>}
    </div>
  );
}
