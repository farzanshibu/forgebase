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
import { explain } from "../../errors";
import { LinkButton } from "./LinkButton";
import { deployedFunctions, liveKeys, newestCollection, userCollections, type ProjectState, type Probe } from "./state";

export type StepId = "admin" | "collection" | "key" | "file" | "function";

export interface Step {
  id: StepId;
  title: string;
  /** true done, false to do, undefined when the server could not tell us. */
  done: boolean | undefined;
  /** One plain sentence: what it is (to do), what exists (done) or why we could not check. */
  detail: string;
  to: string;
  action: string;
}

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;
const enc = encodeURIComponent;

function unknown<T>(p: Probe<T>): string | undefined {
  return p.state === "error" ? `Could not check: ${explain(p.error).title.replace(/\.$/, "")}.` : undefined;
}

/**
 * The checklist for a project. `disabled(studioPath)` hides a step whose studio the server switched off (the nav
 * hides it too), so a server without storage never asks for an upload.
 */
export function gettingStartedSteps(project: ProjectState, opts: { adminEmail?: string; disabled?: (studioPath: string) => boolean } = {}): Step[] {
  const off = (path: string, p: Probe<unknown>) => p.state === "off" || !!opts.disabled?.(path);
  const steps: Step[] = [
    {
      id: "admin",
      title: "Create the administrator",
      done: true,
      detail: opts.adminEmail ? `Done: you are signed in as ${opts.adminEmail}.` : "Done: you are signed in.",
      to: "/auth/admins",
      action: "Administrators",
    },
  ];

  const c = project.collections;
  const mine = c.state === "ok" ? userCollections(c.value) : [];
  const newest = c.state === "ok" ? newestCollection(c.value) : undefined;
  steps.push(
    newest
      ? { id: "collection", title: "Create a collection", done: true, detail: `${plural(mine.length, "collection")}. The newest is ${newest.name}.`, to: `/database/c/${enc(newest.name)}`, action: `Open ${newest.name}` }
      : {
          id: "collection", title: "Create a collection", done: c.state === "ok" ? false : undefined,
          detail: unknown(c) ?? "A collection is a table with a schema. Give it a few fields, then add a record.",
          to: "/database/new", action: "New collection",
        },
  );

  const k = project.keys;
  const keys = k.state === "ok" ? liveKeys(k.value) : [];
  steps.push(
    keys.length > 0
      ? { id: "key", title: "Create an API key", done: true, detail: `${plural(keys.length, "active key")}. Your app sends one with each request.`, to: "/auth/keys", action: "Manage keys" }
      : {
          id: "key", title: "Create an API key", done: k.state === "ok" ? false : undefined,
          detail: unknown(k) ?? "Your app sends an API key with each request. Give it only the access it needs.",
          to: "/auth/keys?new=1", action: "Create API key",
        },
  );

  const s = project.storage;
  if (!off("storage", s)) {
    const stats = s.state === "ok" ? s.value : undefined;
    steps.push(
      stats && stats.objects > 0
        ? { id: "file", title: "Upload a file", done: true, detail: `${plural(stats.objects, "file")} in ${plural(stats.buckets, "bucket")}.`, to: "/storage", action: "Open Storage" }
        : stats && stats.buckets > 0
          ? { id: "file", title: "Upload a file", done: false, detail: "Open a bucket and drop a file into it, such as an avatar or an export.", to: "/storage", action: "Upload a file" }
          : {
              id: "file", title: "Upload a file", done: stats ? false : undefined,
              detail: unknown(s) ?? "Files live in buckets. Make a bucket, then drop a file into it.",
              to: "/storage?new=1", action: "New bucket",
            },
    );
  }

  const f = project.functions;
  if (!off("functions", f)) {
    const live = f.state === "ok" ? deployedFunctions(f.value) : [];
    const latest = live.reduce<(typeof live)[number] | undefined>((b, x) => (!b || x.updated_at > b.updated_at ? x : b), undefined);
    steps.push(
      latest
        ? { id: "function", title: "Deploy a function", done: true, detail: `${plural(live.length, "function")} deployed. The newest is ${latest.name}.`, to: `/functions/f/${enc(latest.name)}`, action: `Open ${latest.name}` }
        : {
            id: "function", title: "Deploy a function", done: f.state === "ok" ? false : undefined,
            detail: unknown(f) ?? "Write a function in the browser that reads your collection, deploy it and test it.",
            to: "/functions/new", action: "New function",
          },
    );
  }
  return steps;
}

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

export function GettingStarted({ project, adminEmail, disabled }: { project: ProjectState | undefined; adminEmail?: string; disabled?: (studioPath: string) => boolean }) {
  const headingId = useId();
  const listId = useId();
  const [collapsed, setCollapsed] = useState(readCollapsed);
  const [showDone, setShowDone] = useState(false);
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
            {done} of {steps.length} done. {next ? `Next: ${next.title.toLowerCase()}.` : "Every step is done."}
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
