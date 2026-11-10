import { Route, Routes, useParams } from "react-router-dom";
import { RouteTabs } from "../panel/RouteTabs";
import { getVectorCollection } from "../../api";
import { ErrorView, Loading } from "../../components/ui";
import { useLoad } from "../../hooks";
import VectorIndex from "./VectorIndex";
import VectorRecords from "./VectorRecords";
import VectorSearch from "./VectorSearch";

export default function VectorCollectionView() {
  const { name = "" } = useParams();
  const { data, error, reload } = useLoad((s) => getVectorCollection(name, s), `vc/${name}`);
  const base = `/vector/c/${encodeURIComponent(name)}`;
  const tabs = [
    { to: base, label: "Records", end: true },
    { to: `${base}/search`, label: "Search", end: false },
    { to: `${base}/index`, label: "Index", end: false },
  ];
  if (error) return <ErrorView error={error} />;
  if (!data) return <Loading what="Loading collection" />;
  return (
    <div>
      <h2 className="text-lg font-medium">{data.name}</h2>
      <p className="text-sm text-muted-foreground">
        {data.description || "No description"} · {data.items} items · fields: {data.fields.map((f) => `${f.name} (${f.dimension}, ${f.metric}, ${f.quantization})`).join(", ") || "none"}
      </p>
      <RouteTabs label="Collection sections" tabs={tabs} className="mt-3" />
      <div className="mt-4">
        <Routes>
          <Route index element={<VectorRecords collection={data} onChanged={reload} />} />
          <Route path="search" element={<VectorSearch collection={data} />} />
          <Route path="index" element={<VectorIndex collection={data} />} />
        </Routes>
      </div>
    </div>
  );
}
