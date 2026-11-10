import { Route, Routes, useParams } from "react-router-dom";
import { RouteTabs } from "../panel/RouteTabs";
import { listCollections } from "../../api";
import { useLoad } from "../../hooks";
import { ErrorView, Loading } from "../../components/ui";
import CollectionGrid from "./CollectionGrid";
import ImportExport from "./ImportExport";
import IndexManager from "./IndexManager";
import RecordEditor from "./RecordEditor";
import SchemaDesigner from "./SchemaDesigner";

const TABS = [
  { to: "", label: "Data", end: true },
  { to: "/schema", label: "Schema", end: false },
  { to: "/indexes", label: "Indexes", end: false },
  { to: "/transfer", label: "Import / export", end: false },
];

export default function CollectionView() {
  const { name = "" } = useParams();
  const { data, error, loading, reload } = useLoad(listCollections, "collections");
  const coll = data?.find((c) => c.name === name);
  return (
    <div>
      <h2 className="text-lg font-medium">{name}</h2>
      <RouteTabs label="Collection sections" tabs={TABS.map((t) => ({ ...t, to: `/database/c/${encodeURIComponent(name)}${t.to}` }))} className="mt-2 mb-4" />
      <ErrorView error={error} />
      {loading && !error && <Loading what="Loading collection" />}
      {data && !coll && <p role="alert" className="text-destructive">No collection named {name}.</p>}
      {coll && (
        <Routes>
          <Route index element={<CollectionGrid collection={coll} />} />
          <Route path="r/:id" element={<RecordEditor collection={coll} />} />
          <Route path="schema" element={<SchemaDesigner existing={coll} onSaved={reload} />} />
          <Route path="indexes" element={<IndexManager collection={coll.name} />} />
          <Route path="transfer" element={<ImportExport collection={coll} />} />
        </Routes>
      )}
    </div>
  );
}
