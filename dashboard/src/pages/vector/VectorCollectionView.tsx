import { NavLink, Route, Routes, useParams } from "react-router-dom";
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
      <nav aria-label="Collection sections" className="mt-3 flex gap-1 border-b border-border">
        {tabs.map((t) => (
          <NavLink key={t.label} to={t.to} end={t.end} className={({ isActive }) => `-mb-px border-b-2 px-3 py-1.5 text-sm ${isActive ? "border-primary text-foreground" : "border-transparent text-muted-foreground hover:text-foreground"}`}>
            {t.label}
          </NavLink>
        ))}
      </nav>
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
