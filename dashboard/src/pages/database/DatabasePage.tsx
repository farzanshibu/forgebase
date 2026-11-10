import { Route, Routes } from "react-router-dom";
import { RouteTabs } from "../panel/RouteTabs";
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
      <RouteTabs label="Database sections" tabs={TABS} className="mt-4" />
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
