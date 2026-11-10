import {
  CodeIcon,
  CreditCardIcon,
  DashboardSquare01Icon,
  Database01Icon,
  FingerPrintIcon,
  Fire03Icon,
  Folder02Icon,
  HierarchySquare01Icon,
  IdentityCardIcon,
  Logout01Icon,
  Queue01Icon,
  ServerStack01Icon,
  Shield01Icon,
  AiNetworkIcon,
  TaskDaily01Icon,
  ZapIcon,
} from "@hugeicons/core-free-icons";
import { HugeiconsIcon, type IconSvgElement } from "@hugeicons/react";
import { NavRail } from "panelui-web/nav-rail";
import type { MouseEvent } from "react";
import { Suspense, lazy } from "react";
import RouteLoading from "./RouteLoading";

import { Outlet, useHref, useLocation, useNavigate } from "react-router-dom";
import ImpersonationBanner from "../auth/ImpersonationBanner";
import { useSession } from "../auth/session";
import { STUDIOS, type Studio } from "../studios";
import { disabledStudioPaths, FeaturesContext, useFeatureData } from "../pages/setup/features";

const ThemeMenu = lazy(() => import("./ThemeMenu"));

const ICONS: Record<string, IconSvgElement> = {
  "": DashboardSquare01Icon,
  database: Database01Icon,
  redis: ZapIcon,
  storage: Folder02Icon,
  vector: AiNetworkIcon,
  graph: HierarchySquare01Icon,
  auth: FingerPrintIcon,
  access: Shield01Icon,
  identity: IdentityCardIcon,
  functions: CodeIcon,
  jobs: TaskDaily01Icon,
  queues: Queue01Icon,
  billing: CreditCardIcon,
  system: ServerStack01Icon,
};

const GROUPS: { label: string; paths: string[] }[] = [
  { label: "Overview", paths: [""] },
  { label: "Data", paths: ["database", "redis", "storage", "vector", "graph"] },
  { label: "Identity and access", paths: ["auth", "access", "identity"] },
  { label: "Compute", paths: ["functions", "jobs", "queues"] },
  { label: "Business", paths: ["billing"] },
  { label: "Operations", paths: ["system"] },
];

const icon = (svg: IconSvgElement) => <HugeiconsIcon icon={svg} size={18} strokeWidth={1.8} aria-hidden="true" />;

function isActive(s: Studio, pathname: string) {
  return s.path === "" ? pathname === "/" : pathname === `/${s.path}` || pathname.startsWith(`/${s.path}/`);
}

/** One studio in the rail: a real link (router href, so open-in-new-tab works) that navigates client-side. */
function StudioItem({ studio, active }: { studio: Studio; active: boolean }) {
  const to = studio.path === "" ? "/" : `/${studio.path}`;
  const href = useHref(to);
  const navigate = useNavigate();
  const onClick = (e: MouseEvent<HTMLAnchorElement>) => {
    if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    e.preventDefault();
    navigate(to);
  };
  return (
    <NavRail.Item href={href} active={active} icon={icon(ICONS[studio.path] ?? DashboardSquare01Icon)} label={studio.label} onClick={onClick}>
      {studio.label}
      {studio.phase !== null && <NavRail.Badge>P{studio.phase}</NavRail.Badge>}
    </NavRail.Item>
  );
}

export default function Layout() {
  const { admin, signOut } = useSession();
  const features = useFeatureData();
  const hidden = disabledStudioPaths(features.snapshot?.features);
  const { pathname } = useLocation();
  const current = STUDIOS.find((s) => s.path !== "" && isActive(s, pathname)) ?? STUDIOS[0];
  return (
    <FeaturesContext.Provider value={features}>
    <NavRail.Provider storageKey="forgebase.admin.rail" className="h-svh min-h-0">
      <div className="flex h-svh w-full bg-background">
        <NavRail aria-label="Studios" className="border-e border-border bg-card">
          <NavRail.Header>
            <NavRail.Brand
              label="ForgeBase admin"
              icon={
                <span className="flex size-7 items-center justify-center rounded-lg bg-primary text-primary-foreground">
                  <HugeiconsIcon icon={Fire03Icon} size={16} strokeWidth={2} aria-hidden="true" />
                </span>
              }
            >
              <span className="font-semibold tracking-tight">ForgeBase</span>
              <span className="ms-1.5 text-xs font-normal text-muted-foreground">admin</span>
            </NavRail.Brand>
            <NavRail.Trigger />
          </NavRail.Header>
          <NavRail.Content>
            {GROUPS.filter((g) => g.paths.some((p) => !hidden.has(p))).map((g) => (
              <NavRail.Group key={g.label} label={g.label}>
                {g.paths.map((p) => {
                  const s = STUDIOS.find((x) => x.path === p);
                  return s && !hidden.has(s.path) ? <StudioItem key={p} studio={s} active={isActive(s, pathname)} /> : null;
                })}
              </NavRail.Group>
            ))}
          </NavRail.Content>
          <NavRail.Footer>
            <NavRail.Separator className="mb-1" />
            <NavRail.Item icon={icon(Logout01Icon)} label="Sign out" className="text-start" onClick={() => void signOut()}>
              Sign out
            </NavRail.Item>
          </NavRail.Footer>
          <NavRail.Rail />
        </NavRail>

        <NavRail.Inset className="min-w-0 bg-background">
          <ImpersonationBanner />
          <header className="flex h-14 shrink-0 items-center gap-2 border-b border-border bg-background/80 px-3 backdrop-blur md:px-5">
            <NavRail.Trigger />
            <div className="min-w-0 flex-1">
              <p className="truncate text-sm font-medium">{current?.label}</p>
              <p className="hidden truncate text-xs text-muted-foreground sm:block">{current?.summary}</p>
            </div>
            <p className="hidden max-w-56 truncate text-xs text-muted-foreground sm:block" title={admin?.email}>
              {admin?.email}
            </p>
            <Suspense fallback={<span className="size-8" aria-hidden="true" />}>
              <ThemeMenu />
            </Suspense>
          </header>
          <main className="min-h-0 min-w-0 flex-1 overflow-y-auto p-4 md:p-8">
            <Suspense fallback={<RouteLoading />}>
              <Outlet />
            </Suspense>
          </main>
        </NavRail.Inset>
      </div>
    </NavRail.Provider>
    </FeaturesContext.Provider>
  );
}
