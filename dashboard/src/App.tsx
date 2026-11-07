import { Route, Routes } from "react-router-dom";
import Layout from "./components/Layout";
import StudioPlaceholder from "./components/StudioPlaceholder";
import DatabasePage from "./pages/database/DatabasePage";
import SystemPage from "./pages/SystemPage";
import { STUDIOS } from "./studios";

export default function App() {
  return (
    <Routes>
      <Route element={<Layout />}>
        {STUDIOS.map((s) =>
          s.path === "system" ? (
            <Route key={s.path} path="system" element={<SystemPage />} />
          ) : s.path === "database" ? (
            <Route key={s.path} path="database/*" element={<DatabasePage />} />
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
