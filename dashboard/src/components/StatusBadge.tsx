/**
 * Status badges and dots in one colour per meaning (see statusTone.ts): green done, blue waiting, amber in progress or
 * needs a look, red failed, grey stopped. Tinted PanelUI badges, small and rounded-md, the same in every studio.
 *
 * Usage:
 *   <Badge status={invoice.status} />                  // "past due" in red with a dot; the words come from the status
 *   <Badge status={job.status} className="capitalize" /> // "Running" in amber with a pulsing dot
 *   <Badge tone="ok" dot>Active</Badge>                // your own words in a tone
 *   <Badge>Custom</Badge>                              // a kind, not a state: outline, no colour
 *   <Dot tone="bad" pulse />Server unreachable         // a bare dot; the words beside it carry the meaning
 *
 * `statusTone(status)` (statusTone.ts) gives the Tone for your own markup.
 */
import { Badge as PanelBadge, type BadgeVariant } from "panelui-web/badge";
import type { ReactNode } from "react";
import { statusLive, statusTone, statusWords, type Tone } from "./statusTone";

export type { Tone } from "./statusTone";

const VARIANT: Record<Tone, BadgeVariant> = { ok: "success", info: "info", warn: "warning", bad: "destructive", neutral: "secondary", default: "outline" };

export interface BadgeProps {
  /** The colour. Defaults to `statusTone(status)` with a status, else the outline "default". */
  tone?: Tone;
  /** A server status word: picks the tone, shows a dot (pulsing while live) and, without children, the words. */
  status?: string;
  /** A dot before the words. On by default with `status`. */
  dot?: boolean;
  /** Make the dot breathe, for a state that is still changing. On by default for running states. */
  pulse?: boolean;
  /** Hover words. */
  title?: string;
  /** "sm" in tables and lists (default), "md" for a headline result. */
  size?: "sm" | "md";
  className?: string;
  children?: ReactNode;
}

/** A state or kind as a small tinted badge. */
export function Badge({ tone, status, dot, pulse, title, size = "sm", className, children }: BadgeProps) {
  const t = tone ?? (status !== undefined ? statusTone(status) : "default");
  const live = pulse ?? (status !== undefined && statusLive(status));
  const showDot = live || (dot ?? status !== undefined);
  return (
    <PanelBadge variant={VARIANT[t]} size={size} dot={live ? "pulse" : showDot} title={title} className={className}>
      {children ?? (status !== undefined ? statusWords(status) : null)}
    </PanelBadge>
  );
}

const DOT: Record<Tone, string> = { ok: "bg-success", warn: "bg-warning", bad: "bg-destructive", info: "bg-info", neutral: "bg-muted-foreground/60", default: "bg-muted-foreground/60" };

/** A small coloured dot; the words next to it carry the meaning. */
export function Dot({ tone, pulse = false, className = "" }: { tone: Tone; pulse?: boolean; className?: string }) {
  return (
    <span aria-hidden="true" className={`relative inline-flex size-2 shrink-0 ${className}`.trim()}>
      {pulse && <span className={`absolute inline-flex size-full animate-ping rounded-full opacity-60 motion-reduce:hidden ${DOT[tone]}`} />}
      <span className={`relative inline-flex size-2 rounded-full ${DOT[tone]}`} />
    </span>
  );
}
