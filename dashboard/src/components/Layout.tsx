/**
 * The app shell: grouped nav rail (Overview, then Build, Users and access, Operate), and a top bar with
 * breadcrumbs, the command palette's Search button and the theme menu (dashboard-ux.md rule 7).
 * The ForgeBase logo is the one control that folds and unfolds the rail (also Ctrl+B / ⌘B); on a narrow screen,
 * where the rail is a drawer, the top bar has a menu button instead. The account (who is signed in, Sign out) sits
 * at the foot of the rail.
 * Studios whose feature is switched off (FB-92) are hidden from the rail and the palette by one filter,
 * `visibleStudios` in studios.ts.
 *
 * Pages render inside <main> and draw their own PageHeader; the shell never repeats the studio name.
 */
import { Fire03Icon, Logout01Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon, type IconSvgElement } from "@hugeicons/react";
import { studioIcon } from "./studioIcons";
import { NavRail, useNavRail } from "panelui-web/nav-rail";
import { Suspense, lazy, useEffect, type MouseEvent } from "react";
import { Outlet, useHref, useLocation, useNavigate } from "react-router-dom";
import ImpersonationBanner from "../auth/ImpersonationBanner";
import { useSession } from "../auth/session";
import { FeaturesContext, useFeatureData } from "../pages/setup/features";
import { GROUPS, isStudioDisabled, studio as studioByPath, studioAt, studioHref, visibleStudios, type Studio } from "../studios";
import { useAdminScope } from "../auth/scope";
import { setToastNavigator } from "../toaster";
import { Breadcrumbs, CrumbsProvider } from "./Breadcrumbs";
import { CommandPalette } from "./CommandPalette";
import { JourneyGuide } from "./JourneyGuide";
import RouteLoading from "./RouteLoading";

const ThemeMenu = lazy(() => import("./ThemeMenu"));


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
    <NavRail.Item href={href} active={active} icon={icon(studioIcon(studio.path))} label={studio.label} onClick={onClick} className="h-9">
      {studio.label}
    </NavRail.Item>
  );
}

const isMac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform);
const TOGGLE_KEYS = isMac ? "⌘B" : "Ctrl+B";

/** True while focus is somewhere the user types, so a shortcut must not fire. */
function typing(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  return !!el && (el.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName));
}

/**
 * The logo at the top of the rail: it folds and unfolds the rail (there is no separate collapse button).
 * In the mobile drawer it closes the drawer. Ctrl+B / ⌘B does the same from anywhere outside a text field.
 */
function BrandToggle() {
  const { collapsed, toggle, isMobile, setMobileOpen } = useNavRail();
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key.toLowerCase() !== "b" || !(e.ctrlKey || e.metaKey) || e.altKey || e.shiftKey || typing(e.target)) return;
      e.preventDefault();
      toggle();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [toggle]);
  const action = isMobile ? "Close navigation" : collapsed ? "Expand navigation" : "Collapse navigation";
  return (
    <button
      type="button"
      onClick={isMobile ? () => setMobileOpen(false) : toggle}
      aria-label={`ForgeBase admin: ${action.toLowerCase()}`}
      aria-expanded={isMobile ? true : !collapsed}
      title={isMobile ? action : `${action} (${TOGGLE_KEYS})`}
      className="group/brand flex min-w-0 flex-1 cursor-pointer items-center gap-2.5 rounded-[10px] px-1.5 py-1 text-start outline-none transition-colors duration-150 hover:bg-foreground/[0.05] focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset group-data-[collapsed]/nav-rail:flex-none group-data-[collapsed]/nav-rail:justify-center group-data-[collapsed]/nav-rail:px-0"
    >
      <span className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-primary text-primary-foreground shadow-sm transition-transform duration-150 group-hover/brand:scale-105 group-active/brand:scale-95" aria-hidden="true">
        <HugeiconsIcon icon={Fire03Icon} size={17} strokeWidth={2} />
      </span>
      <span className="flex min-w-0 items-baseline gap-1.5 group-data-[collapsed]/nav-rail:hidden" aria-hidden="true">
        <span className="truncate text-base font-semibold tracking-tight">ForgeBase</span>
        <span className="rounded-md bg-muted px-1.5 py-px text-xs font-medium text-muted-foreground">admin</span>
      </span>
    </button>
  );
}

/** The top bar's menu button, only while the rail is a drawer (narrow screens): there the logo is out of sight. */
function MobileNavButton() {
  const { isMobile } = useNavRail();
  return isMobile ? <NavRail.Trigger expandLabel="Open navigation" collapseLabel="Close navigation" /> : null;
}

function initials(email: string | undefined): string {
  const name = (email ?? "").split("@")[0] ?? "";
  const parts = name.split(/[._+-]+/).filter(Boolean);
  const two = parts.length > 1 ? parts[0]![0]! + parts[1]![0]! : name.slice(0, 2);
  return two.toUpperCase() || "?";
}

/** Who is signed in, and Sign out: the foot of the rail. Folded, only the avatar and the Sign out icon remain. */
function Account({ email, scope, onSignOut }: { email?: string; scope: string; onSignOut: () => void }) {
  return (
    <div className="flex items-center gap-2.5 rounded-[10px] px-1.5 py-1 group-data-[collapsed]/nav-rail:flex-col group-data-[collapsed]/nav-rail:gap-1 group-data-[collapsed]/nav-rail:px-0">
      <span
        className="flex size-8 shrink-0 items-center justify-center rounded-full bg-primary/12 text-xs font-semibold tracking-wide text-primary ring-1 ring-primary/20"
        title={email ? `${email} · ${scope}` : scope}
        aria-hidden="true"
      >
        {initials(email)}
      </span>
      <div className="min-w-0 flex-1 leading-tight group-data-[collapsed]/nav-rail:hidden">
        <p className="truncate text-sm font-medium" title={email}>
          {email}
        </p>
        <p className="truncate text-xs text-muted-foreground">{scope}</p>
      </div>
      <button
        type="button"
        onClick={onSignOut}
        aria-label="Sign out"
        title="Sign out"
        className="inline-flex size-8 shrink-0 cursor-pointer items-center justify-center rounded-lg text-muted-foreground outline-none transition-colors hover:bg-destructive/10 hover:text-destructive focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
      >
        <HugeiconsIcon icon={Logout01Icon} size={17} strokeWidth={1.8} aria-hidden="true" />
      </button>
    </div>
  );
}

export default function Layout() {
  const { admin, signOut } = useSession();
  const features = useFeatureData();
  const { platform, realm } = useAdminScope();
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
                <BrandToggle />
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
                <Account email={admin?.email} scope={platform ? "Instance administrator" : `Realm administrator · ${realm ?? "default"}`} onSignOut={() => void signOut()} />
              </NavRail.Footer>
            </NavRail>

            <NavRail.Inset className="min-w-0 bg-background">
              <ImpersonationBanner />
              <header className="flex h-14 shrink-0 items-center gap-2 border-b border-border bg-background/80 px-3 backdrop-blur md:px-6">
                <MobileNavButton />
                <div className="min-w-0 flex-1">
                  <Breadcrumbs />
                </div>
                <JourneyGuide disabled={(path) => isStudioDisabled(studioByPath(path), features.snapshot?.features)} />
                <CommandPalette studios={studios} />
                <Suspense fallback={<span className="size-8" aria-hidden="true" />}>
                  <ThemeMenu />
                </Suspense>
              </header>
              <main className="min-h-0 min-w-0 flex-1 overflow-y-auto px-4 py-6 md:px-8 md:py-8">
                {/* One readable width for every studio; wide screens get margins instead of 2000 px tables. */}
                <div className="mx-auto w-full max-w-[1440px]">
                  <Suspense fallback={<RouteLoading />}>
                    <Outlet />
                  </Suspense>
                </div>
              </main>
            </NavRail.Inset>
          </div>
        </NavRail.Provider>
      </CrumbsProvider>
    </FeaturesContext.Provider>
  );
}
