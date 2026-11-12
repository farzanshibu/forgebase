/**
 * JourneyGuide: the Getting started checklist (Overview) as a small button in the top bar, on every page until the
 * first journey is done (dashboard-ux.md, "The test": create a collection, a key, upload a file, deploy a function).
 * It shows how far along the project is and opens a menu of the steps with the next one first in line, so the next
 * thing to do is always one click away, wherever the user is.
 *
 * - Hidden on Overview (the full checklist is there), once every step is done, and after "Hide this guide".
 * - Reads the same state as Overview (overview/state.ts, overview/model.ts) and reloads when a save, create or delete
 *   toast fires (CHANGED_EVENT) or the tab comes back into view, so finishing a step moves it on at once.
 *
 * Usage (the Layout does this):
 *   <JourneyGuide disabled={(path) => isStudioDisabled(studio(path), features)} />
 */
import { ArrowRight01Icon, CheckmarkCircle02Icon, CircleIcon, Rocket01Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { Menu } from "panelui-web/menu";
import { useCallback, useEffect, useRef, useState } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { useSession } from "../auth/session";
import { gettingStartedSteps, midSentence } from "../pages/overview/model";
import { loadProjectState, type ProjectState } from "../pages/overview/state";
import { CHANGED_EVENT } from "../toaster";

const HIDE_KEY = "forgebase.admin.guide.hidden";

function readHidden(): boolean {
  try {
    return localStorage.getItem(HIDE_KEY) === "1";
  } catch {
    return false;
  }
}

function writeHidden() {
  try {
    localStorage.setItem(HIDE_KEY, "1");
  } catch {
    // Storage blocked: hidden until the page reloads.
  }
}

/** A small progress ring: `done` of `total` as an arc in the accent colour. */
function Ring({ done, total }: { done: number; total: number }) {
  const r = 7;
  const c = 2 * Math.PI * r;
  return (
    <svg viewBox="0 0 18 18" className="size-[18px] -rotate-90" aria-hidden="true">
      <circle cx="9" cy="9" r={r} fill="none" stroke="var(--border)" strokeWidth="2.25" />
      <circle cx="9" cy="9" r={r} fill="none" stroke="var(--primary)" strokeWidth="2.25" strokeLinecap="round" strokeDasharray={`${(done / total) * c} ${c}`} />
    </svg>
  );
}

export function JourneyGuide({ disabled }: { disabled?: (studioPath: string) => boolean }) {
  const { admin } = useSession();
  const { pathname } = useLocation();
  const navigate = useNavigate();
  const [hidden, setHidden] = useState(readHidden);
  const [project, setProject] = useState<ProjectState>();
  const live = useRef(true);

  const load = useCallback(() => {
    void loadProjectState().then(
      (p) => live.current && setProject(p),
      () => undefined,
    );
  }, []);

  useEffect(() => {
    live.current = true;
    if (hidden) return;
    load();
    let timer: ReturnType<typeof setTimeout> | undefined;
    // Several toasts in a row (a bulk upload) reload once.
    const soon = () => {
      clearTimeout(timer);
      timer = setTimeout(load, 400);
    };
    const visible = () => document.visibilityState === "visible" && soon();
    window.addEventListener(CHANGED_EVENT, soon);
    document.addEventListener("visibilitychange", visible);
    return () => {
      live.current = false;
      clearTimeout(timer);
      window.removeEventListener(CHANGED_EVENT, soon);
      document.removeEventListener("visibilitychange", visible);
    };
  }, [hidden, load]);

  if (hidden || !project || pathname === "/" || pathname === "") return null;
  const steps = gettingStartedSteps(project, { adminEmail: admin?.email, disabled });
  const done = steps.filter((s) => s.done).length;
  if (done === steps.length) return null;
  const next = steps.find((s) => !s.done);

  return (
    <Menu>
      <Menu.Trigger
        render={
          <button
            type="button"
            aria-label={`Getting started: ${done} of ${steps.length} done${next ? `. Next: ${midSentence(next.title)}` : ""}`}
            className="hidden h-8 cursor-pointer items-center gap-2 rounded-lg border border-border bg-background px-2.5 text-sm text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring md:inline-flex"
          />
        }
      >
        <Ring done={done} total={steps.length} />
        <span className="hidden lg:inline">Getting started</span>
        <span className="font-medium tabular-nums text-foreground">
          {done}/{steps.length}
        </span>
      </Menu.Trigger>
      <Menu.Content align="end" className="w-80">
        <Menu.Group>
          <Menu.GroupLabel className="flex items-center gap-2">
            <HugeiconsIcon icon={Rocket01Icon} size={14} strokeWidth={1.8} aria-hidden="true" />
            Getting started · {done} of {steps.length} done
          </Menu.GroupLabel>
          {steps.map((s, i) => {
            const isNext = s === next;
            return (
              <Menu.Item key={s.id} onClick={() => navigate(s.to)} disabled={s.id === "admin"} className="items-start gap-2.5 py-2">
                <HugeiconsIcon
                  icon={s.done ? CheckmarkCircle02Icon : CircleIcon}
                  size={16}
                  strokeWidth={s.done ? 2 : 1.6}
                  // `!`: the menu tints item icons; a done step stays green, the next one takes the accent.
                  className={`mt-0.5 shrink-0 ${s.done ? "text-success!" : isNext ? "text-primary!" : "text-muted-foreground"}`}
                  aria-hidden="true"
                />
                <span className="min-w-0 flex-1 whitespace-normal">
                  <span className={`block text-sm ${s.done ? "text-muted-foreground line-through decoration-muted-foreground/40" : isNext ? "font-medium" : ""}`}>
                    <span className="sr-only">{s.done ? "Done: " : "To do: "}</span>
                    {i + 1}. {s.title}
                  </span>
                  {isNext && (
                    <>
                      <span className="mt-0.5 block text-xs leading-snug text-muted-foreground">{s.detail}</span>
                      <span className="mt-1.5 inline-flex items-center gap-1 rounded-md bg-primary px-2 py-0.5 text-xs font-medium text-primary-foreground">
                        {s.action}
                        <HugeiconsIcon icon={ArrowRight01Icon} size={12} strokeWidth={2} aria-hidden="true" className="text-primary-foreground!" />
                      </span>
                    </>
                  )}
                </span>
              </Menu.Item>
            );
          })}
        </Menu.Group>
        <Menu.Separator />
        <Menu.Item onClick={() => navigate("/")}>Open Overview</Menu.Item>
        <Menu.Item
          onClick={() => {
            writeHidden();
            setHidden(true);
          }}
        >
          Hide this guide
        </Menu.Item>
      </Menu.Content>
    </Menu>
  );
}

export default JourneyGuide;
