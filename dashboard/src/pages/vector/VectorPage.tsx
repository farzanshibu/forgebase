import { NavLink, Route, Routes } from "react-router-dom";
import VectorCollectionView from "./VectorCollectionView";
import VectorCollections from "./VectorCollections";

export default function VectorPage() {
  return (
    <div>
      <h1 className="text-2xl font-semibold tracking-tight">Vector Studio</h1>
      <nav aria-label="Vector sections" className="mt-4 flex gap-1 border-b border-border">
        <NavLink to="/vector" end className={({ isActive }) => `-mb-px border-b-2 px-3 py-2 text-sm ${isActive ? "border-primary text-foreground" : "border-transparent text-muted-foreground hover:text-foreground"}`}>
          Collections
        </NavLink>
      </nav>
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
