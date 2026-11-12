/**
 * Pieces shared by the Authentication and Access studios: avatars, people, principals and dialogs. Table cards,
 * badges, times, chips, ids and facts are the shared components in src/components (TableCard, StatusBadge, Time,
 * Chips, IdText, Facts); import them from there. The plain-word helpers (times, events, user agents) are in words.tsx.
 *
 * Usage:
 *   <Person name="ann@example.com" sub={<IdText id="u1" what="user id" />} />
 *   <PrincipalText value="admin:a1" admins={emails} />
 *   <SecretModal secret={key} label="API key" name="ci" onClose={…} />
 */
import { Dialog } from "panelui-web/dialog";
import type { ReactNode } from "react";
import { Button, CopyButton } from "../../components/ui";
import { initialsName, principalParts } from "./words";

// ---- People and principals ----------------------------------------------------------------------------------------

const TINTS = [
  "bg-chart-1/12 text-chart-1 ring-chart-1/20",
  "bg-chart-2/12 text-chart-2 ring-chart-2/20",
  "bg-chart-3/12 text-chart-3 ring-chart-3/20",
  "bg-chart-4/12 text-chart-4 ring-chart-4/20",
  "bg-chart-5/12 text-chart-5 ring-chart-5/20",
];
const SIZES = { sm: "size-8 text-[0.6875rem]", md: "size-10 text-xs", lg: "size-12 text-sm" } as const;

/** Initials on a tint picked from the name, so one person keeps one colour. Decorative: the name is always written beside it. */
export function Initials({ name, size = "sm", shape = "circle", icon }: { name: string; size?: keyof typeof SIZES; shape?: "circle" | "square"; icon?: ReactNode }) {
  let hash = 0;
  for (const ch of name) hash = (hash * 31 + ch.codePointAt(0)!) | 0;
  const words = initialsName(name).split(/\s+/).filter(Boolean);
  const letters = ((words[0]?.[0] ?? "") + (words.length > 1 ? words[words.length - 1]![0] : "")).toUpperCase() || "?";
  return (
    <span aria-hidden="true" className={`flex shrink-0 items-center justify-center font-semibold tracking-wide ring-1 ring-inset ${shape === "circle" ? "rounded-full" : "rounded-lg"} ${SIZES[size]} ${TINTS[Math.abs(hash) % TINTS.length]}`}>
      {icon ?? letters}
    </span>
  );
}

/** An avatar with the initials, a primary line and an optional muted second line. */
export function Person({ name, sub, shape = "circle", fallback, children }: { name: string; sub?: ReactNode; shape?: "circle" | "square"; fallback?: ReactNode; children?: ReactNode }) {
  return (
    <span className="flex min-w-0 items-center gap-2.5">
      <Initials name={name} shape={shape} icon={fallback} />
      <span className="flex min-w-0 flex-col">
        <span className="truncate font-medium text-foreground">{children ?? name}</span>
        {sub && <span className="flex min-w-0 items-center text-xs text-muted-foreground">{sub}</span>}
      </span>
    </span>
  );
}

/** A principal as a kind label and a readable name: an administrator's e-mail when known, else the id in mono. */
export function PrincipalText({ value, admins }: { value: string; admins?: Map<string, string> }) {
  const p = principalParts(value);
  const email = p.kind === "admin" ? admins?.get(p.id) : undefined;
  return (
    <span className="flex min-w-0 items-center gap-1.5" title={value}>
      {p.label && <span className="shrink-0 text-xs text-muted-foreground">{p.label}</span>}
      {email ? <span className="truncate">{email}</span> : <span className="truncate font-mono text-xs">{p.id}</span>}
    </span>
  );
}

// ---- Dialogs ------------------------------------------------------------------------------------------------------

/** PanelUI dialog, open while mounted. Without `onClose` it can only be dismissed by what the children offer (a secret shown once). */
export function Modal({ title, description, onClose, footer, size = "md", children }: { title: string; description?: ReactNode; onClose?: () => void; footer?: ReactNode; size?: "sm" | "md" | "lg"; children: ReactNode }) {
  return (
    <Dialog open onOpenChange={(open) => { if (!open) onClose?.(); }} dismissible={false}>
      <Dialog.Content size={size} showClose={Boolean(onClose)}>
        <Dialog.Header title={title} description={description} />
        <Dialog.Body>{children}</Dialog.Body>
        {footer && <Dialog.Footer>{footer}</Dialog.Footer>}
      </Dialog.Content>
    </Dialog>
  );
}

/** Shows a secret exactly once; the parent drops it when the dialog closes. */
export function SecretModal({ secret, label, name, onClose, children }: { secret: string; label: string; name?: string; onClose: () => void; children?: ReactNode }) {
  return (
    <Modal
      title={`${label} created`}
      size="lg"
      description={name ? <>“{name}” is ready. Copy the secret and store it somewhere safe, such as your app’s environment variables.</> : "Copy the secret and store it somewhere safe."}
      footer={
        <Button tone="primary" onClick={onClose}>
          Done
        </Button>
      }
    >
      <div className="min-w-0 space-y-3">
        <p role="note" className="flex items-start gap-2 rounded-lg border border-warning/40 bg-warning/10 px-3 py-2 text-sm text-foreground">
          <span aria-hidden="true" className="mt-0.5 font-semibold text-warning">!</span>
          <span>
            <strong className="font-semibold">Copy it now.</strong> It is shown once and cannot be retrieved later. If you lose it, revoke it and make a new one.
          </span>
        </p>
        <div className="flex min-w-0 items-stretch gap-2">
          <code aria-label="Secret" className="block min-w-0 flex-1 select-all break-all rounded-lg border border-border bg-muted/50 px-3 py-2 font-mono text-xs leading-relaxed">
            {secret}
          </code>
          <CopyButton text={secret} label="Copy secret" className="h-auto shrink-0" />
        </div>
        {children}
      </div>
    </Modal>
  );
}
