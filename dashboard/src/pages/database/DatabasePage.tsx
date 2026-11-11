import { Outlet, Route, Routes, useLocation, useNavigate } from "react-router-dom";
import { RouteTabs } from "../../components/controls";
import { PageHeader } from "../../components/PageHeader";
import { Button } from "../../components/ui";
import { useLoad } from "../../hooks";
import BackendInfo from "./BackendInfo";
import CollectionBrowser from "./CollectionBrowser";
import CollectionView from "./CollectionView";
import Extensions from "./Extensions";
import IndexManager from "./IndexManager";
import { getDatabaseInfo } from "./info";
import Logins from "./Logins";
import PgOnly from "./PgOnly";
import Policies from "./Policies";
import SchemaDesigner from "./SchemaDesigner";
import SlowQueries from "./SlowQueries";
import SqlEditor from "./SqlEditor";

const TABS = [
  { to: "/database", label: "Collections", end: true },
  { to: "/database/sql", label: "SQL editor", end: false },
  { to: "/database/indexes", label: "Indexes", end: false },
  { to: "/database/slow", label: "Slow queries", end: false },
];
/** Tabs that only mean something on PostgreSQL (FB-155): hidden on SQLite, and a direct link says what is needed. */
const PG_TABS = [
  { to: "/database/policies", label: "Policies", end: false },
  { to: "/database/logins", label: "Direct logins", end: false },
  { to: "/database/extensions", label: "Extensions", end: false },
];

/** Header, backend line and tabs for the studio's own pages. A collection and the new-collection form bring their own header. */
function Section() {
  const nav = useNavigate();
  const { pathname } = useLocation();
  const { data: info } = useLoad(getDatabaseInfo, "database-info");
  const tabs = info?.backend === "postgres" ? [...TABS, ...PG_TABS] : TABS;
  const onLogins = pathname.startsWith("/database/logins");
  return (
    <div>
      <PageHeader
        studio="database"
        primary={onLogins
          ? <Button tone="primary" onClick={() => nav("/database/logins?new=1")}>New direct login</Button>
          : <Button tone="primary" onClick={() => nav("/database/new")}>New collection</Button>}
      >
        <BackendInfo info={info} />
      </PageHeader>
      <RouteTabs label="Database sections" tabs={tabs} className="mt-4" />
      <div className="mt-6"><Outlet /></div>
    </div>
  );
}

/** A PostgreSQL-only page: waits for the backend answer, then shows the page or what it needs. */
function Pg({ what, children }: { what: string; children: React.ReactNode }) {
  const { data: info } = useLoad(getDatabaseInfo, "database-info");
  return <PgOnly info={info} what={what}>{children}</PgOnly>;
}

export default function DatabasePage() {
  return (
    <Routes>
      <Route element={<Section />}>
        <Route index element={<CollectionBrowser />} />
        <Route path="sql" element={<SqlEditor />} />
        <Route path="indexes" element={<IndexManager />} />
        <Route path="slow" element={<SlowQueries />} />
        <Route path="policies" element={<Pg what="Policies"><Policies /></Pg>} />
        <Route path="logins" element={<Pg what="Direct logins"><Logins /></Pg>} />
        <Route path="extensions" element={<Pg what="Extensions"><Extensions /></Pg>} />
      </Route>
      <Route path="new" element={<SchemaDesigner />} />
      <Route path="c/:name/*" element={<CollectionView />} />
      <Route path="*" element={<p className="text-muted-foreground">No such page in the Database studio.</p>} />
    </Routes>
  );
}
