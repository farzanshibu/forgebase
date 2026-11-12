/**
 * SaveBar: the bar a long form keeps its Save (or Next) button in. One implementation, so every form pins it the
 * same way (dashboard-ux.md rule 5).
 *
 * Inside the app shell it is drawn in the shell's footer slot, under the scrolling page: it sits on the bottom edge
 * of the main area however far the page is scrolled and never covers the form. (A `sticky bottom-0` bar inside the
 * page could not do that: the page's scroll area has padding, a sticky box stops at that padding, and the form
 * showed through the gap under the bar.) Outside the shell (first-run setup, a component test) there is no slot and
 * it is a sticky bar at the end of the form.
 *
 * Usage:
 *   <SaveBar label="Save changes" className="max-w-5xl">   // className: the width of the form above it
 *     <span className="text-sm text-muted-foreground">2 unsaved changes</span>
 *     <div className="flex gap-2"><Button>Discard</Button><Button tone="primary">Save changes</Button></div>
 *   </SaveBar>
 *
 * Children are laid out in a row, first at the start and last at the end. `label` names the region for screen readers.
 */
import { useContext, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { SaveBarSlotContext } from "./saveBarSlot";

const ROW = "flex flex-wrap items-center justify-between gap-3 py-3";

export function SaveBar({ label, className = "", children }: { label: string; className?: string; children: ReactNode }) {
  const slot = useContext(SaveBarSlotContext);
  if (!slot) {
    return (
      <div role="region" aria-label={label} className={`sticky bottom-0 z-10 -mx-1 border-t border-border bg-background/95 px-1 backdrop-blur ${ROW}`}>
        {children}
      </div>
    );
  }
  return createPortal(
    // The same gutters and 1440px column as the page above (Layout's <main>), so the bar lines up with the form.
    <div role="region" aria-label={label} className="border-t border-border bg-card px-4 md:px-8">
      <div className="mx-auto w-full max-w-[1440px]">
        <div className={`${ROW} ${className}`.trim()}>{children}</div>
      </div>
    </div>,
    slot,
  );
}
