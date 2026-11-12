/**
 * EmptyState: an empty list that teaches (dashboard-ux.md rule 2, audit A-09). Says what the thing is, why you
 * would want one, and offers the primary action, an optional sample, and a docs link. Never just "No items".
 *
 * Usage:
 *   <EmptyState
 *     title="No buckets yet"
 *     why="Buckets hold files your app uploads, like avatars or exports."
 *     action={<Button tone="primary" onClick={() => setCreating(true)}>New bucket</Button>}
 *     sample={<Button onClick={loadSample}>Try a sample</Button>}
 *     docs="docs/adr/0018-object-storage.md"
 *   />
 *   <EmptyState compact title="No users match" why="Clear the search to see every user." />   // filtered list
 *
 * `icon` takes any Hugeicons icon; the default is a neutral box.
 */
import { PackageOpenIcon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon, type IconSvgElement } from "@hugeicons/react";
import type { ReactNode } from "react";
import { DocsLink } from "./PageHeader";

export interface EmptyStateProps {
  /** What is missing, in sentence case: "No functions yet". */
  title: string;
  /** One sentence: what the thing is and why you would create one. */
  why: ReactNode;
  /** The primary action, usually the same button as the page header's. */
  action?: ReactNode;
  /** A secondary action such as "Try a sample". */
  sample?: ReactNode;
  /** Repo-relative docs page (see studios.ts `docsHref`). */
  docs?: string;
  icon?: IconSvgElement;
  /** Smaller, for a filtered list or a panel inside a page. */
  compact?: boolean;
  className?: string;
}

export function EmptyState({ title, why, action, sample, docs, icon = PackageOpenIcon, compact = false, className = "" }: EmptyStateProps) {
  return (
    <section
      aria-label={title}
      className={`flex flex-col items-center rounded-xl border border-dashed border-border text-center ${compact ? "gap-1.5 px-4 py-6" : "gap-2 bg-card/50 px-6 py-12"} ${className}`.trim()}
    >
      {!compact && (
        <span
          className="mb-2 flex size-12 items-center justify-center rounded-xl border border-border bg-card text-primary shadow-sm ring-4 ring-primary/5"
          aria-hidden="true"
        >
          <HugeiconsIcon icon={icon} size={22} strokeWidth={1.7} />
        </span>
      )}
      <h2 className={compact ? "text-sm font-medium" : "text-base font-semibold tracking-tight"}>{title}</h2>
      <p className="max-w-md text-sm text-muted-foreground">{why}</p>
      {(action || sample || docs) && (
        <div className="mt-3 flex flex-wrap items-center justify-center gap-2">
          {action}
          {sample}
          {docs && <DocsLink doc={docs} />}
        </div>
      )}
    </section>
  );
}

export default EmptyState;
