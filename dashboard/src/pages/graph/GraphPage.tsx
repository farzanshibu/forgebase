import { NavLink, Route, Routes } from "react-router-dom";
import EdgeBrowser from "./EdgeBrowser";
import GraphExplorer from "./GraphExplorer";
import GraphOverview from "./GraphOverview";
import NodeBrowser from "./NodeBrowser";
import QueryRunner from "./QueryRunner";

const TABS = [
  { to: "/graph", label: "Graphs", end: true },
  { to: "/graph/nodes", label: "Nodes", end: false },
  { to: "/graph/edges", label: "Edges", end: false },
  { to: "/graph/explorer", label: "Explorer", end: false },
  { to: "/graph/query", label: "Query runner", end: false },
];

export default function GraphPage() {
  return (
    <div>
      <h1 className="text-2xl font-semibold tracking-tight">Graph Studio</h1>
      <nav aria-label="Graph sections" className="mt-4 flex flex-wrap gap-1 border-b border-border">
        {TABS.map((t) => (
          <NavLink key={t.label} to={t.to} end={t.end} className={({ isActive }) => `-mb-px border-b-2 px-3 py-2 text-sm ${isActive ? "border-primary text-foreground" : "border-transparent text-muted-foreground hover:text-foreground"}`}>
            {t.label}
          </NavLink>
        ))}
      </nav>
      <div className="mt-6">
        <Routes>
          <Route index element={<GraphOverview />} />
          <Route path="nodes" element={<NodeBrowser />} />
          <Route path="edges" element={<EdgeBrowser />} />
          <Route path="explorer" element={<GraphExplorer />} />
          <Route path="query" element={<QueryRunner />} />
          <Route path="*" element={<p className="text-muted-foreground">No such page in Graph Studio.</p>} />
        </Routes>
      </div>
    </div>
  );
}
