import { Outlet, Route, Routes, useNavigate } from "react-router-dom";
import { RouteTabs } from "../../components/controls";
import { PageHeader } from "../../components/PageHeader";
import { Button } from "../../components/ui";
import BackendInfo from "./BackendInfo";
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

/** Header, backend line and tabs for the studio's own pages. A collection and the new-collection form bring their own header. */
function Section() {
  const nav = useNavigate();
  return (
    <div>
      <PageHeader studio="database" primary={<Button tone="primary" onClick={() => nav("/database/new")}>New collection</Button>}>
        <BackendInfo />
      </PageHeader>
      <RouteTabs label="Database sections" tabs={TABS} className="mt-4" />
      <div className="mt-6"><Outlet /></div>
    </div>
  );
}

export default function DatabasePage() {
  return (
    <Routes>
      <Route element={<Section />}>
        <Route index element={<CollectionBrowser />} />
        <Route path="sql" element={<SqlEditor />} />
        <Route path="indexes" element={<IndexManager />} />
        <Route path="slow" element={<SlowQueries />} />
      </Route>
      <Route path="new" element={<SchemaDesigner />} />
      <Route path="c/:name/*" element={<CollectionView />} />
      <Route path="*" element={<p className="text-muted-foreground">No such page in the Database studio.</p>} />
    </Routes>
  );
}
