/**
 * HelpTip: a small "?" button that explains a term from src/glossary.ts (dashboard-ux.md rule 8, audit A-15).
 * Reachable by keyboard; the definition shows on hover and focus, and is also tied to the button with
 * `aria-describedby`, so a screen reader reads it without opening anything.
 *
 * Usage:
 *   <label>TTL <HelpTip term="ttl" /></label>
 *   <HelpTip term="rls">Row-level security</HelpTip>        // the words, then the "?"
 *   <FormField label="Ack deadline" term="ack_deadline">…</FormField>   // FormField adds it for you
 *
 * Add new terms to glossary.ts; do not pass free text.
 */
import { Tooltip } from "panelui-web/tooltip";
import { useId, type ReactNode } from "react";
import { GLOSSARY, type GlossaryTerm } from "../glossary";

export function HelpTip({ term, children }: { term: GlossaryTerm; children?: ReactNode }) {
  const entry = GLOSSARY[term];
  const id = `help${useId().replace(/:/g, "")}`;
  const tip = (
    <Tooltip>
      <Tooltip.Trigger
        type="button"
        aria-label={`What is ${entry.short}?`}
        aria-describedby={id}
        className="inline-flex size-5 shrink-0 items-center justify-center rounded-full border border-border text-xs font-semibold leading-none text-muted-foreground hover:text-foreground focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring"
      >
        ?
      </Tooltip.Trigger>
      <Tooltip.Content variant="rich" className="max-w-xs">
        <Tooltip.Title>{entry.short}</Tooltip.Title>
        <Tooltip.Description>{entry.text}</Tooltip.Description>
      </Tooltip.Content>
      <span id={id} hidden>
        {entry.text}
      </span>
    </Tooltip>
  );
  if (!children) return tip;
  return (
    <span className="inline-flex items-center gap-1">
      {children}
      {tip}
    </span>
  );
}

export default HelpTip;
