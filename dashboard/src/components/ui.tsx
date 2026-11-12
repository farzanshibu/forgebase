/**
 * Small shared pieces every studio uses. The bigger patterns live in their own files (see docs/ux/components.md).
 *
 * Usage:
 *   <Button tone="primary">Save</Button>
 *   <ConfirmButton label="Delete bucket docs" confirmText="docs" consequence="Its files are deleted too." onConfirm={…} />
 *   <ConfirmButton label={`Archive price ${key}`} short="Archive" onConfirm={…} />   // short words, full name for screen readers
 *   <IconButton icon={Delete02Icon} label={`Delete ${name}`} tone="danger" onClick={…} />   // icon only; label = name and tooltip
 *   <ErrorView error={e} />          // = ErrorPanel: plain words, next step, request id, Details
 *   <Panel title="Buckets" actions={…}>…</Panel>
 *   <Loading what="Loading keys" />  // small inline wait; lists and tables use Skeleton
 */
import type { ComponentProps, ReactNode } from "react";
import { HugeiconsIcon, type IconSvgElement } from "@hugeicons/react";
import { Button as PanelButton } from "panelui-web/button";
import { Card } from "panelui-web/card";
import { Spinner } from "panelui-web/spinner";
import { Button } from "./Button";
import { ConfirmDialog } from "./ConfirmDialog";
import { ErrorPanel, FeatureDisabled } from "./ErrorPanel";

export { Button, CopyButton, CopyIcon } from "./Button";
export { FeatureDisabled };

/**
 * One-line destructive button: opens ConfirmDialog, and `onConfirm` runs only from its action. Kept so studios can
 * move to ConfirmDialog gradually; new code passes `consequence` (what is lost) and, for an irreversible action,
 * `confirmText` (the name to type). `short` is the visible text of a row action ("Archive"); `label` ("Archive price
 * pro_month") stays its accessible name, and the dialog's question and confirm button use it.
 */
export function ConfirmButton({ label, short, onConfirm, disabled, consequence = "This cannot be undone.", confirmText, className }: { label: string; short?: string; onConfirm: () => void | Promise<unknown>; disabled?: boolean; consequence?: ReactNode; confirmText?: string; className?: string }) {
  return (
    <ConfirmDialog
      trigger={<Button tone="danger" aria-label={short !== undefined && short !== label ? label : undefined} disabled={disabled} className={className} />}
      triggerLabel={short ?? label}
      title={`${label}?`}
      consequence={consequence}
      confirmLabel={`Confirm ${label.toLowerCase()}`}
      confirmText={confirmText}
      onConfirm={onConfirm}
    />
  );
}

/**
 * A square, quiet button that holds only an icon, for row actions. `label` is its accessible name and tooltip;
 * `tone="danger"` colours it as destructive on hover. Works as a ConfirmDialog trigger.
 */
export function IconButton({ icon, label, tone = "default", className = "", ...p }: Omit<ComponentProps<"button">, "children"> & { icon: IconSvgElement; label: string; tone?: "default" | "danger" }) {
  const color = tone === "danger" ? "text-muted-foreground hover:not-disabled:bg-destructive/10 hover:not-disabled:text-destructive" : "text-muted-foreground hover:not-disabled:text-foreground";
  return (
    <PanelButton type="button" variant="ghost" size="icon-sm" aria-label={label} title={label} {...p} className={`${color} ${className}`.trim()}>
      <HugeiconsIcon icon={icon} size={16} strokeWidth={1.8} aria-hidden="true" className="shrink-0" />
    </PanelButton>
  );
}

/** The error display every screen uses: ErrorPanel under its old name. */
export const ErrorView = ErrorPanel;

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
            <Card.Title as="h2" className="text-base font-semibold tracking-tight text-foreground">
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
