import { Route, Routes } from "react-router-dom";
import { RouteTabs } from "../../components/controls";
import VectorCollectionView from "./VectorCollectionView";
import VectorCollections from "./VectorCollections";

export default function VectorPage() {
  return (
    <div>
      <h1 className="text-2xl font-semibold tracking-tight">Vector Studio</h1>
      <RouteTabs label="Vector sections" tabs={[{ to: "/vector", label: "Collections", end: true }]} className="mt-4" />
      <div className="mt-6">
        <Routes>
          <Route index element={<VectorCollections />} />
          <Route path="c/:name/*" element={<VectorCollectionView />} />
          <Route path="*" element={<p className="text-muted-foreground">No such page in Vector Studio.</p>} />
        </Routes>
      </div>
    </div>
  );
}
