import { Outlet, Route, Routes, useNavigate } from "react-router-dom";
import { RouteTabs } from "../../components/controls";
import { PageHeader } from "../../components/PageHeader";
import { UseFromCodeButton } from "../../components/UseFromCode";
import { Button } from "../../components/ui";
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

function Section() {
  const nav = useNavigate();
  return (
    <div>
      <PageHeader
        studio="graph"
        secondary={<UseFromCodeButton kind="graph" name="" />}
        primary={<Button tone="primary" onClick={() => nav("/graph/nodes?new=1")}>New node</Button>}
      />
      <RouteTabs label="Graph sections" tabs={TABS} className="mt-4" />
      <div className="mt-6"><Outlet /></div>
    </div>
  );
}

export default function GraphPage() {
  return (
    <Routes>
      <Route element={<Section />}>
        <Route index element={<GraphOverview />} />
        <Route path="nodes" element={<NodeBrowser />} />
        <Route path="edges" element={<EdgeBrowser />} />
        <Route path="explorer" element={<GraphExplorer />} />
        <Route path="query" element={<QueryRunner />} />
        <Route path="*" element={<p className="text-muted-foreground">No such page in the Graph studio.</p>} />
      </Route>
    </Routes>
  );
}
