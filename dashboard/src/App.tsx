import { lazy, Suspense, type ComponentType } from "react";
import { Link, Navigate, Route, Routes, useLocation } from "react-router-dom";
import { SessionProvider, useSession } from "./auth/session";
import Layout from "./components/Layout";
import RouteLoading from "./components/RouteLoading";
import { Toast } from "panelui-web/toast";
import { Compass01Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import ThemeProvider from "./theme/ThemeProvider";
import { toaster } from "./toaster";
import { useSetupStatus } from "./pages/setup/useSetupStatus";

// Each studio is its own chunk, fetched when its route is first opened.
const LoginPage = lazy(() => import("./pages/LoginPage"));
const OverviewPage = lazy(() => import("./pages/OverviewPage"));
const AccessPage = lazy(() => import("./pages/access/AccessPage"));
const BillingPage = lazy(() => import("./pages/billing/BillingPage"));
const AuthPage = lazy(() => import("./pages/auth/AuthPage"));
const DatabasePage = lazy(() => import("./pages/database/DatabasePage"));
const IdentityPage = lazy(() => import("./pages/identity/IdentityPage"));
const FunctionsPage = lazy(() => import("./pages/functions/FunctionsPage"));
const JobsPage = lazy(() => import("./pages/jobs/JobsPage"));
const QueuesPage = lazy(() => import("./pages/queues/QueuesPage"));
const RedisPage = lazy(() => import("./pages/redis/RedisPage"));
const StoragePage = lazy(() => import("./pages/storage/StoragePage"));
const GraphPage = lazy(() => import("./pages/graph/GraphPage"));
const VectorPage = lazy(() => import("./pages/vector/VectorPage"));
const SetupPage = lazy(() => import("./pages/setup/SetupPage"));
const SystemStudio = lazy(() => import("./pages/system/SystemStudio"));
// Dev builds only: every shared component on one page (docs/ux/components.md). Vite drops it from production.
const PatternsPage = import.meta.env.DEV ? lazy(() => import("./components/PatternsPage")) : null;

/** Studio routes, by path in studios.ts. A studio's page owns everything under its path. */
const PAGES: { path: string; Page: ComponentType }[] = [
  { path: "database", Page: DatabasePage },
  { path: "storage", Page: StoragePage },
  { path: "functions", Page: FunctionsPage },
  { path: "redis", Page: RedisPage },
  { path: "vector", Page: VectorPage },
  { path: "graph", Page: GraphPage },
  { path: "queues", Page: QueuesPage },
  { path: "jobs", Page: JobsPage },
  { path: "auth", Page: AuthPage },
  { path: "access", Page: AccessPage },
  { path: "identity", Page: IdentityPage },
  { path: "billing", Page: BillingPage },
  { path: "system", Page: SystemStudio },
];

export default function App() {
  return (
    <ThemeProvider>
      <Toast.Provider toastManager={toaster} position="bottom-right">
        <SessionProvider>
          <Gate />
        </SessionProvider>
      </Toast.Provider>
    </ThemeProvider>
  );
}

function Gate() {
  const { status } = useSession();
  const { pathname } = useLocation();
  if (status !== "loading" && pathname.replace(/\/+$/, "") === "/setup")
    return (
      <Suspense fallback={<RouteLoading variant="screen" />}>
        <SetupPage />
      </Suspense>
    );
  if (status === "loading") return <RouteLoading variant="screen" />;
  if (status === "anonymous") return <SignIn />;
  return <Studios />;
}

/** Sign-in, or the first-run setup while the server has no administrator. */
function SignIn() {
  const setup = useSetupStatus();
  if (setup.loading) return <RouteLoading variant="screen" />;
  if (setup.required) return <Navigate to="/setup" replace />;
  return (
    <Suspense fallback={<RouteLoading variant="screen" />}>
      <LoginPage />
    </Suspense>
  );
}

function Studios() {
  return (
    <Routes>
      <Route element={<Layout />}>
        <Route index element={<OverviewPage />} />
        {PAGES.map(({ path, Page }) => (
          <Route key={path} path={`${path}/*`} element={<Page />} />
        ))}
        {PatternsPage && <Route path="_patterns" element={<PatternsPage />} />}
        <Route path="*" element={<NotFound />} />
      </Route>
    </Routes>
  );
}

function NotFound() {
  return (
    <div className="mx-auto flex max-w-lg flex-col items-center py-16 text-center">
      <span className="mb-5 flex size-14 items-center justify-center rounded-2xl border border-border bg-card text-muted-foreground shadow-sm" aria-hidden="true">
        <HugeiconsIcon icon={Compass01Icon} size={26} strokeWidth={1.6} />
      </span>
      <p className="font-mono text-xs font-medium uppercase tracking-widest text-primary">404</p>
      <h1 className="mt-2 text-2xl font-semibold tracking-tight">Page not found</h1>
      <p className="mt-2 text-sm text-muted-foreground">
        This address is not a page in the dashboard. Pick a studio from the sidebar, press <kbd className="rounded border border-border bg-muted px-1 py-px text-xs">Ctrl+K</kbd> (⌘K on a Mac) to search, or go back to{" "}
        <Link to="/" className="font-medium text-primary underline-offset-2 hover:underline">
          Overview
        </Link>
        .
      </p>
    </div>
  );
}
