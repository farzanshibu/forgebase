import { Navigate, Route, Routes, useNavigate, useParams } from "react-router-dom";
import { RouteTabs } from "../../components/controls";
import { listCollections } from "../../api";
import { useLoad } from "../../hooks";
import { PageHeader } from "../../components/PageHeader";
import { SkeletonTable } from "../../components/Skeleton";
import { UseFromCodeButton } from "../../components/UseFromCode";
import { Button, ErrorView } from "../../components/ui";
import CollectionGrid from "./CollectionGrid";
import ImportExport from "./ImportExport";
import IndexManager from "./IndexManager";
import SchemaDesigner from "./SchemaDesigner";

const TABS = [
  { to: "", label: "Data", end: true },
  { to: "/schema", label: "Schema", end: false },
  { to: "/indexes", label: "Indexes", end: false },
  { to: "/transfer", label: "Import / export", end: false },
];

export default function CollectionView() {
  const { name = "" } = useParams();
  const nav = useNavigate();
  const { data, error, loading, reload } = useLoad(listCollections, "collections");
  const coll = data?.find((c) => c.name === name);
  const base = `/database/c/${encodeURIComponent(name)}`;
  return (
    <div>
      <PageHeader
        title={name}
        description={coll ? `${coll.type} collection · ${coll.fields.filter((f) => !f.system).length} fields · ${coll.schema_mode ?? "strict"} schema` : "Collection"}
        docs="docs/database.md"
        secondary={<UseFromCodeButton kind="collection" name={name} />}
        primary={<Button tone="primary" onClick={() => nav(`${base}?record=new`)}>Add record</Button>}
      />
      <RouteTabs label="Collection sections" tabs={TABS.map((t) => ({ ...t, to: `${base}${t.to}` }))} className="mt-4 mb-4" />
      <ErrorView error={error} onRetry={reload} />
      {loading && !error && <SkeletonTable rows={4} columns={4} label="Loading collection" />}
      {data && !coll && <p role="alert" className="text-destructive">No collection named {name}.</p>}
      {coll && (
        <Routes>
          <Route index element={<CollectionGrid collection={coll} collections={data ?? []} />} />
          <Route path="r/:id" element={<LegacyRecord base={base} />} />
          <Route path="schema" element={<SchemaDesigner existing={coll} onSaved={reload} />} />
          <Route path="indexes" element={<IndexManager collection={coll.name} />} />
          <Route path="transfer" element={<ImportExport collection={coll} />} />
        </Routes>
      )}
    </div>
  );
}

/** Old deep links `/c/x/r/<id>` open the record sheet on the grid. */
function LegacyRecord({ base }: { base: string }) {
  const { id = "" } = useParams();
  return <Navigate replace to={`${base}?record=${encodeURIComponent(id)}`} />;
}
