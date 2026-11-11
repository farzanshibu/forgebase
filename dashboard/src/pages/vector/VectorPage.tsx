import { Route, Routes } from "react-router-dom";
import VectorCollectionView from "./VectorCollectionView";
import VectorCollections from "./VectorCollections";

/** One page, so no tab row (A-22): add a RouteTabs here when the studio gets a second section. */
export default function VectorPage() {
  return (
    <Routes>
      <Route index element={<VectorCollections />} />
      <Route path="c/:name/*" element={<VectorCollectionView />} />
      <Route path="*" element={<p className="text-muted-foreground">No such page in the Vector studio.</p>} />
    </Routes>
  );
}
