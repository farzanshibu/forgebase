/**
 * Getting started: the first journey from dashboard-ux.md ("The test") as a checklist read from the server (audit
 * A-02). Each step says what it is, whether it is done, and links in one click to the place to do it.
 *
 * Usage:
 *   const project = useProjectState();
 *   <GettingStarted project={project.data} adminEmail={admin?.email} disabled={(s) => isStudioDisabled(studio(s), features)} />
 *
 * Collapsing is remembered per browser. Once every step is done the checklist folds into one line with "Show again".
 */
import { useId, useState, type ReactNode } from "react";
import { Alert02Icon, ArrowDown01Icon, ArrowUp01Icon, CheckmarkCircle02Icon, CircleIcon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { Card } from "panelui-web/card";
import { Button } from "../../components/ui";
import { Skeleton } from "../../components/Skeleton";
import { LinkButton } from "./LinkButton";
import { gettingStartedSteps, midSentence } from "./model";
import type { ProjectState } from "./state";

const COLLAPSE_KEY = "forgebase.admin.gettingStarted.collapsed";

function readCollapsed(): boolean {
  try {
    return localStorage.getItem(COLLAPSE_KEY) === "1";
  } catch {
    return false;
  }
}

function writeCollapsed(v: boolean) {
  try {
    if (v) localStorage.setItem(COLLAPSE_KEY, "1");
    else localStorage.removeItem(COLLAPSE_KEY);
  } catch {
    /* storage blocked: the choice lasts for this visit only */
  }
}

function StepIcon({ done }: { done: boolean | undefined }) {
  if (done) return <HugeiconsIcon icon={CheckmarkCircle02Icon} size={20} strokeWidth={2} className="text-success" aria-hidden="true" />;
  if (done === undefined) return <HugeiconsIcon icon={Alert02Icon} size={20} strokeWidth={2} className="text-warning" aria-hidden="true" />;
  return <HugeiconsIcon icon={CircleIcon} size={20} strokeWidth={1.8} className="text-muted-foreground" aria-hidden="true" />;
}

function Shell({ children, headingId }: { children: ReactNode; headingId: string }) {
  return (
    <Card as="section" aria-labelledby={headingId} className="rounded-xl">
      {children}
    </Card>
  );
}

export interface GettingStartedProps {
  project: ProjectState | undefined;
  adminEmail?: string;
  disabled?: (studioPath: string) => boolean;
  /** Controlled "Show again" (Overview widens its layout while the finished checklist is folded); internal when omitted. */
  showDone?: boolean;
  onShowDoneChange?: (show: boolean) => void;
}

export function GettingStarted({ project, adminEmail, disabled, showDone: shown, onShowDoneChange }: GettingStartedProps) {
  const headingId = useId();
  const listId = useId();
  const [collapsed, setCollapsed] = useState(readCollapsed);
  const [ownShowDone, setOwnShowDone] = useState(false);
  const showDone = shown ?? ownShowDone;
  const setShowDone = (v: boolean) => (onShowDoneChange ? onShowDoneChange(v) : setOwnShowDone(v));
  const toggle = () => {
    setCollapsed((c) => {
      writeCollapsed(!c);
      return !c;
    });
  };

  if (!project) {
    return (
      <Shell headingId={headingId}>
        <Card.Header>
          <Card.Title as="h2" id={headingId} className="text-base font-semibold">Getting started</Card.Title>
        </Card.Header>
        <Card.Content>
          <div role="status" aria-label="Loading Getting started" className="space-y-3">
            {[0, 1, 2, 3, 4].map((i) => <Skeleton key={i} className="h-10 w-full" />)}
          </div>
        </Card.Content>
      </Shell>
    );
  }

  const steps = gettingStartedSteps(project, { adminEmail, disabled });
  const done = steps.filter((s) => s.done).length;
  const allDone = done === steps.length;
  const next = steps.find((s) => !s.done);

  if (allDone && !showDone) {
    return (
      <Shell headingId={headingId}>
        <Card.Content className="flex flex-wrap items-center justify-between gap-3 py-4">
          <div className="flex items-center gap-2">
            <HugeiconsIcon icon={CheckmarkCircle02Icon} size={20} strokeWidth={2} className="text-success" aria-hidden="true" />
            <h2 id={headingId} className="text-sm font-medium">Getting started</h2>
            <p className="text-sm text-muted-foreground">All {steps.length} steps are done.</p>
          </div>
          <Button onClick={() => setShowDone(true)}>Show again</Button>
        </Card.Content>
      </Shell>
    );
  }

  const pct = Math.round((done / steps.length) * 100);
  return (
    <Shell headingId={headingId}>
      <Card.Header className="flex flex-row flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <Card.Title as="h2" id={headingId} className="text-base font-semibold">Getting started</Card.Title>
          <Card.Description>
            {done} of {steps.length} done. {next ? `Next: ${midSentence(next.title)}.` : "Every step is done."}
          </Card.Description>
          <div role="progressbar" aria-label="Getting started progress" aria-valuenow={done} aria-valuemin={0} aria-valuemax={steps.length} aria-valuetext={`${done} of ${steps.length} steps done`} className="mt-2 h-1.5 w-48 max-w-full overflow-hidden rounded-full bg-muted">
            <div className="h-full rounded-full bg-success transition-[width]" style={{ width: `${pct}%` }} />
          </div>
        </div>
        <div className="flex items-center gap-2">
          {allDone && <Button onClick={() => setShowDone(false)}>Hide</Button>}
          <Button aria-expanded={!collapsed} aria-controls={listId} onClick={toggle}>
            <HugeiconsIcon icon={collapsed ? ArrowDown01Icon : ArrowUp01Icon} size={16} strokeWidth={1.8} aria-hidden="true" />
            {collapsed ? "Show steps" : "Hide steps"}
          </Button>
        </div>
      </Card.Header>
      <Card.Content id={listId} hidden={collapsed}>
        <ol aria-label="Steps" className="divide-y divide-border">
          {steps.map((s, i) => (
            <li key={s.id} className="flex flex-wrap items-center gap-x-3 gap-y-2 py-3 first:pt-0 last:pb-0 sm:flex-nowrap">
              <StepIcon done={s.done} />
              <div className="min-w-0 flex-1">
                <p className={`text-sm font-medium ${s.done ? "text-muted-foreground" : ""}`}>
                  <span className="sr-only">{s.done ? "Done: " : s.done === undefined ? "Not checked: " : "To do: "}</span>
                  {i + 1}. {s.title}
                </p>
                <p className="text-xs text-muted-foreground">{s.detail}</p>
              </div>
              {s.id !== "admin" && (
                <LinkButton to={s.to} tone={s === next ? "primary" : s.done ? "ghost" : "default"} className="shrink-0">
                  {s.action}
                </LinkButton>
              )}
            </li>
          ))}
        </ol>
      </Card.Content>
    </Shell>
  );
}

export default GettingStarted;
