import { NavLink, Route, Routes } from "react-router-dom";
import CollectionBrowser from "./CollectionBrowser";
import CollectionView from "./CollectionView";
import IndexManager from "./IndexManager";
import SchemaDesigner from "./SchemaDesigner";
import SlowQueries from "./SlowQueries";
import SqlEditor from "./SqlEditor";

const TABS = [
  { to: "/database", label: "Collections", end: true },
  { to: "/database/sql", label: "SQL editor", end: false },
  { to: "/database/indexes", label: "Indexes", end: false },
  { to: "/database/slow", label: "Slow queries", end: false },
];

export default function DatabasePage() {
  return (
    <div>
      <h1 className="text-2xl font-semibold tracking-tight">Database Studio</h1>
      <nav aria-label="Database sections" className="mt-4 flex gap-1 border-b border-border">
        {TABS.map((t) => (
          <NavLink
            key={t.label}
            to={t.to}
            end={t.end}
            className={({ isActive }) =>
              `-mb-px border-b-2 px-3 py-2 text-sm ${isActive ? "border-primary text-foreground" : "border-transparent text-muted-foreground hover:text-foreground"}`
            }
          >
            {t.label}
          </NavLink>
        ))}
      </nav>
      <div className="mt-6">
        <Routes>
          <Route index element={<CollectionBrowser />} />
          <Route path="new" element={<SchemaDesigner />} />
          <Route path="c/:name/*" element={<CollectionView />} />
          <Route path="sql" element={<SqlEditor />} />
          <Route path="indexes" element={<IndexManager />} />
          <Route path="slow" element={<SlowQueries />} />
          <Route path="*" element={<p className="text-muted-foreground">No such page in Database Studio.</p>} />
        </Routes>
      </div>
    </div>
  );
}
