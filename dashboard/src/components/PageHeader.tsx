/**
 * PageHeader: the top of every studio page (dashboard-ux.md rule 1, audit A-10). Studio name as the h1, one plain
 * sentence on what it does, a docs link, and the primary action at the top right.
 *
 * Usage:
 *   <PageHeader studio="queues" primary={<Button tone="primary" onClick={() => setCreating(true)}>New topic</Button>} />
 *   <PageHeader studio="functions" />                       // the studio's glossary term shows as a "?" after the title
 *   <PageHeader title="orders" term="topic" description="Topic" docs="docs/adr/0022-queues-pubsub.md"
 *               primary={<Button>Publish</Button>} secondary={<UseFromCodeButton kind="topic" name="orders" />} />
 *
 * With `studio`, title, description and docs come from studios.ts; any prop you pass wins. The docs link points at
 * GitHub at the running server's version tag (decision Q2). Put RouteTabs under it, not inside it.
 */
import { ArrowUpRight01Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import type { ReactNode } from "react";
import type { GlossaryTerm } from "../glossary";
import { useServerVersion } from "../hooks";
import { HelpTip } from "./HelpTip";
import { docsHref, studio as studioByPath } from "../studios";

export interface PageHeaderProps {
  /** A studios.ts path ("database", "system/features"): fills title, description and docs. */
  studio?: string;
  title?: ReactNode;
  /** A glossary term to explain next to the title ("?" button). With `studio`, the studio's own term is used unless you pass one; `null` hides it. */
  term?: GlossaryTerm | null;
  description?: ReactNode;
  /** Repo-relative docs page; `null` hides the link. */
  docs?: string | null;
  /** The one primary action, top right. */
  primary?: ReactNode;
  /** Quieter actions before it, such as "Use from code" or an overflow menu. */
  secondary?: ReactNode;
  /** Extra content under the description, such as a status badge. */
  children?: ReactNode;
  className?: string;
}

export function PageHeader({ studio, title, term, description, docs, primary, secondary, children, className = "" }: PageHeaderProps) {
  const s = studio !== undefined ? studioByPath(studio) : undefined;
  const heading = title ?? s?.title;
  const text = description ?? s?.summary;
  const helpTerm = term === null ? undefined : (term ?? s?.term);
  const doc = docs === null ? undefined : (docs ?? s?.docs);
  return (
    <header className={`flex flex-wrap items-start justify-between gap-x-6 gap-y-3 ${className}`.trim()}>
      <div className="min-w-0 max-w-3xl">
        <div className="flex items-center gap-2">
          <h1 className="text-2xl font-semibold tracking-tight">{heading}</h1>
          {helpTerm && <HelpTip term={helpTerm} />}
        </div>
        {(text || doc) && (
          <p className="mt-1 text-sm text-muted-foreground">
            {text}
            {doc && (
              <>
                {text ? " " : null}
                <DocsLink doc={doc} />
              </>
            )}
          </p>
        )}
        {children}
      </div>
      {(primary || secondary) && (
        <div className="flex shrink-0 flex-wrap items-center gap-2">
          {secondary}
          {primary}
        </div>
      )}
    </header>
  );
}

/** "Docs" link to the GitHub docs at the running version. Opens in a new tab and says so. */
export function DocsLink({ doc, label = "Docs" }: { doc: string; label?: string }) {
  const version = useServerVersion();
  return (
    <a
      href={docsHref(doc, version)}
      target="_blank"
      rel="noreferrer"
      className="inline-flex items-center gap-0.5 whitespace-nowrap font-medium text-primary underline-offset-2 hover:underline"
    >
      {label}
      <HugeiconsIcon icon={ArrowUpRight01Icon} size={14} strokeWidth={2} aria-hidden="true" />
      <span className="sr-only"> (opens in a new tab)</span>
    </a>
  );
}

export default PageHeader;
