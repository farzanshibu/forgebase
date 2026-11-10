import { lazy, Suspense } from "react";
import { Route, Routes } from "react-router-dom";
import { SessionProvider, useSession } from "./auth/session";
import Layout from "./components/Layout";
import RouteLoading from "./components/RouteLoading";
import StudioPlaceholder from "./components/StudioPlaceholder";
import { STUDIOS } from "./studios";
import { Spinner } from "panelui-web/spinner";
import { Toast } from "panelui-web/toast";
import ThemeProvider from "./theme/ThemeProvider";
import { toaster } from "./toaster";

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
const SystemStudio = lazy(() => import("./pages/system/SystemStudio"));

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
  if (status === "loading")
    return (
      <p className="flex min-h-svh items-center justify-center gap-2 text-sm text-muted-foreground">
        <Spinner aria-hidden="true" role="presentation" /> Loading…
      </p>
    );
  if (status === "anonymous")
    return (
      <Suspense fallback={<RouteLoading />}>
        <LoginPage />
      </Suspense>
    );
  return <Studios />;
}

function Studios() {
  return (
    <Routes>
      <Route element={<Layout />}>
        {STUDIOS.map((s) =>
          s.path === "system" ? (
            <Route key={s.path} path="system/*" element={<SystemStudio />} />
          ) : s.path === "database" ? (
            <Route key={s.path} path="database/*" element={<DatabasePage />} />
          ) : s.path === "auth" ? (
            <Route key={s.path} path="auth/*" element={<AuthPage />} />
          ) : s.path === "access" ? (
            <Route key={s.path} path="access/*" element={<AccessPage />} />
          ) : s.path === "vector" ? (
            <Route key={s.path} path="vector/*" element={<VectorPage />} />
          ) : s.path === "graph" ? (
            <Route key={s.path} path="graph/*" element={<GraphPage />} />
          ) : s.path === "identity" ? (
            <Route key={s.path} path="identity/*" element={<IdentityPage />} />
          ) : s.path === "functions" ? (
            <Route key={s.path} path="functions/*" element={<FunctionsPage />} />
          ) : s.path === "jobs" ? (
            <Route key={s.path} path="jobs/*" element={<JobsPage />} />
          ) : s.path === "queues" ? (
            <Route key={s.path} path="queues/*" element={<QueuesPage />} />
          ) : s.path === "billing" ? (
            <Route key={s.path} path="billing/*" element={<BillingPage />} />
          ) : s.path === "redis" ? (
            <Route key={s.path} path="redis/*" element={<RedisPage />} />
          ) : s.path === "storage" ? (
            <Route key={s.path} path="storage/*" element={<StoragePage />} />
          ) : s.path === "" ? (
            <Route key="index" index element={<OverviewPage />} />
          ) : (
            <Route key={s.path} path={s.path} element={<StudioPlaceholder studio={s} />} />
          ),
        )}
        <Route path="*" element={<NotFound />} />
      </Route>
    </Routes>
  );
}

function NotFound() {
  return (
    <div className="p-8">
      <h1 className="text-xl font-semibold tracking-tight">Page not found</h1>
      <p className="mt-2 text-muted-foreground">Pick a section from the sidebar.</p>
    </div>
  );
}
