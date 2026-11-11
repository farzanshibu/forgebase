/**
 * The app shell: grouped nav rail (Overview, then Build, Users and access, Operate), and a top bar with
 * breadcrumbs, the command palette's Search button, the account and the theme menu (dashboard-ux.md rule 7).
 * Studios whose feature is switched off (FB-92) are hidden from the rail and the palette by one filter,
 * `visibleStudios` in studios.ts.
 *
 * Pages render inside <main> and draw their own PageHeader; the shell never repeats the studio name.
 */
import {
  AiNetworkIcon,
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
  Settings02Icon,
  Shield01Icon,
  TaskDaily01Icon,
  ZapIcon,
} from "@hugeicons/core-free-icons";
import { HugeiconsIcon, type IconSvgElement } from "@hugeicons/react";
import { NavRail } from "panelui-web/nav-rail";
import { Suspense, lazy, useEffect, type MouseEvent } from "react";
import { Outlet, useHref, useLocation, useNavigate } from "react-router-dom";
import ImpersonationBanner from "../auth/ImpersonationBanner";
import { useSession } from "../auth/session";
import { FeaturesContext, useFeatureData } from "../pages/setup/features";
import { GROUPS, studioAt, studioHref, visibleStudios, type Studio } from "../studios";
import { useAdminScope } from "../auth/scope";
import { setToastNavigator } from "../toaster";
import { Breadcrumbs, CrumbsProvider } from "./Breadcrumbs";
import { CommandPalette } from "./CommandPalette";
import RouteLoading from "./RouteLoading";

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
  "system/features": Settings02Icon,
};

const icon = (svg: IconSvgElement) => <HugeiconsIcon icon={svg} size={18} strokeWidth={1.8} aria-hidden="true" />;

/** One studio in the rail: a real link (router href, so open-in-new-tab works) that navigates client-side. */
function StudioItem({ studio, active }: { studio: Studio; active: boolean }) {
  const to = studioHref(studio);
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
    </NavRail.Item>
  );
}

export default function Layout() {
  const { admin, signOut } = useSession();
  const features = useFeatureData();
  const { platform } = useAdminScope();
  const studios = visibleStudios(features.snapshot?.features, { platform });
  const { pathname } = useLocation();
  const navigate = useNavigate();
  useEffect(() => setToastNavigator(navigate), [navigate]);
  const current = studioAt(pathname)?.path;
  const overview = studios.find((s) => s.group === null);
  return (
    <FeaturesContext.Provider value={features}>
      <CrumbsProvider>
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
                {overview && (
                  <NavRail.Group>
                    <StudioItem studio={overview} active={current === overview.path} />
                  </NavRail.Group>
                )}
                {GROUPS.map((g) => {
                  const items = studios.filter((s) => s.group === g.id);
                  if (items.length === 0) return null;
                  return (
                    <NavRail.Group key={g.id} label={g.label}>
                      {items.map((s) => (
                        <StudioItem key={s.path} studio={s} active={current === s.path} />
                      ))}
                    </NavRail.Group>
                  );
                })}
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
                <Breadcrumbs className="flex-1" />
                <CommandPalette studios={studios} />
                <p className="hidden max-w-56 truncate text-xs text-muted-foreground lg:block" title={admin?.email}>
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
      </CrumbsProvider>
    </FeaturesContext.Provider>
  );
}
