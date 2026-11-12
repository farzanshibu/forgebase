import { Link, Route, Routes } from "react-router-dom";
import { EmptyState } from "../../components/EmptyState";
import VectorCollectionView from "./VectorCollectionView";
import VectorCollections from "./VectorCollections";

/** One page, so no tab row (A-22): add a RouteTabs here when the studio gets a second section. */
export default function VectorPage() {
  return (
    <Routes>
      <Route index element={<VectorCollections />} />
      <Route path="c/:name/*" element={<VectorCollectionView />} />
      <Route path="*" element={<EmptyState compact title="No such page in Vector" why="The link may be old or mistyped." action={<Link className="text-sm font-medium text-primary hover:underline" to="/vector">See all collections</Link>} />} />
    </Routes>
  );
}
