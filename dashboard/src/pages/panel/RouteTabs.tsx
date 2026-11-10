import type { ReactNode } from "react";
import { matchPath, useLocation, useNavigate } from "react-router-dom";
import { Tabs } from "panelui-web/tabs";

export interface RouteTab {
  /** Destination; may carry a query string (it is ignored when matching the location). */
  to: string;
  label: ReactNode;
  /** Match the path exactly, as NavLink's `end`. */
  end?: boolean;
}

const pathOf = (to: string) => to.split(/[?#]/)[0] ?? to;

/**
 * PanelUI tabs driven by the router: the active tab comes from the current location (the most specific
 * matching `to` wins), and choosing a tab navigates, so URLs, deep links and Back keep working.
 * A row wider than its space scrolls sideways with a faded edge (PanelUI's own behaviour).
 */
export function RouteTabs({ tabs, label, className }: { tabs: RouteTab[]; label: string; className?: string }) {
  const { pathname } = useLocation();
  const navigate = useNavigate();
  let active: string | null = null;
  let best = -1;
  for (const t of tabs) {
    const p = pathOf(t.to);
    if (p.length > best && matchPath({ path: p, end: t.end ?? false }, pathname)) {
      active = t.to;
      best = p.length;
    }
  }
  return (
    <Tabs variant="underline" value={active} onValueChange={(v) => { if (typeof v === "string" && v !== active) navigate(v); }} className={`gap-0 ${className ?? ""}`.trim()}>
      <Tabs.List aria-label={label}>
        {tabs.map((t) => (
          <Tabs.Trigger key={t.to} value={t.to}>{t.label}</Tabs.Trigger>
        ))}
      </Tabs.List>
    </Tabs>
  );
}
