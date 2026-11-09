import { Route, Routes } from "react-router-dom";
import { SessionProvider, useSession } from "./auth/session";
import Layout from "./components/Layout";
import LoginPage from "./pages/LoginPage";
import StudioPlaceholder from "./components/StudioPlaceholder";
import AccessPage from "./pages/access/AccessPage";
import BillingPage from "./pages/billing/BillingPage";
import AuthPage from "./pages/auth/AuthPage";
import DatabasePage from "./pages/database/DatabasePage";
import FunctionsPage from "./pages/functions/FunctionsPage";
import JobsPage from "./pages/jobs/JobsPage";
import GraphPage from "./pages/graph/GraphPage";
import VectorPage from "./pages/vector/VectorPage";
import SystemPage from "./pages/SystemPage";
import { STUDIOS } from "./studios";

export default function App() {
  return (
    <SessionProvider>
      <Gate />
    </SessionProvider>
  );
}

function Gate() {
  const { status } = useSession();
  if (status === "loading") return <p className="p-8 text-sm text-zinc-500">Loading…</p>;
  if (status === "anonymous") return <LoginPage />;
  return <Studios />;
}

function Studios() {
  return (
    <Routes>
      <Route element={<Layout />}>
        {STUDIOS.map((s) =>
          s.path === "system" ? (
            <Route key={s.path} path="system" element={<SystemPage />} />
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
          ) : s.path === "functions" ? (
            <Route key={s.path} path="functions/*" element={<FunctionsPage />} />
          ) : s.path === "jobs" ? (
            <Route key={s.path} path="jobs/*" element={<JobsPage />} />
          ) : s.path === "billing" ? (
            <Route key={s.path} path="billing/*" element={<BillingPage />} />
          ) : s.path === "" ? (
            <Route key="index" index element={<StudioPlaceholder studio={s} />} />
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
      <h1 className="text-xl font-semibold">Page not found</h1>
      <p className="mt-2 text-zinc-400">Pick a section from the sidebar.</p>
    </div>
  );
}
