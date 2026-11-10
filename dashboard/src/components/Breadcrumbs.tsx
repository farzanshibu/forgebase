/**
 * Breadcrumbs: where you are, fed from the route (dashboard-ux.md rule 7, audit A-06). The top bar shows them;
 * pages do not draw their own "← All topics" links.
 *
 * Built from the URL and studios.ts:
 *   /                          Overview
 *   /queues                    Queues
 *   /queues/subscriptions      Queues › Subscriptions
 *   /queues/t/orders           Queues › Topics › orders
 *   /database/c/posts/schema   Database › Collections › posts › Schema
 *
 * A page that knows better names (a record's title instead of its id) overrides the trail (useCrumbs is in crumbs.ts):
 *   useCrumbs([{ label: "Database", to: "/database" }, { label: "posts", to: "/database/c/posts" }, { label: record.title }]);
 * The last crumb is the current page and is not a link.
 */
import { Breadcrumb } from "panelui-web/breadcrumb";
import { useContext, useMemo, useState, type ReactNode } from "react";
import { Link, useLocation } from "react-router-dom";
import { CrumbContext, crumbsFor, finish, type CrumbOverride } from "./crumbs";

/** Holds a page's override. The Layout mounts it around the top bar and the outlet. */
export function CrumbsProvider({ children }: { children: ReactNode }) {
  const [override, set] = useState<CrumbOverride>();
  const value = useMemo(() => ({ override, set }), [override]);
  return <CrumbContext.Provider value={value}>{children}</CrumbContext.Provider>;
}

export function Breadcrumbs({ className = "" }: { className?: string }) {
  const { pathname } = useLocation();
  const ctx = useContext(CrumbContext);
  const trail = ctx?.override && ctx.override.path === pathname ? finish(ctx.override.crumbs) : crumbsFor(pathname);
  return (
    <Breadcrumb size="sm" maxItems={5} className={`min-w-0 ${className}`.trim()}>
      <Breadcrumb.List className="flex-nowrap">
        {trail.map((c, i) => (
          <Breadcrumb.Item key={`${i}:${c.label}`} className="min-w-0">
            {c.to ? (
              <Breadcrumb.Link render={<Link to={c.to} />} className="truncate">
                {c.label}
              </Breadcrumb.Link>
            ) : (
              <Breadcrumb.Page className="truncate font-medium text-foreground">{c.label}</Breadcrumb.Page>
            )}
          </Breadcrumb.Item>
        ))}
      </Breadcrumb.List>
    </Breadcrumb>
  );
}

export default Breadcrumbs;
