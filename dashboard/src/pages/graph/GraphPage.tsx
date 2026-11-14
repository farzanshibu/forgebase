import { Outlet, Route, Routes, useLocation, useNavigate } from "react-router-dom";
import { EmptyState } from "../../components/EmptyState";
import { RouteTabs } from "../../components/controls";
import { PageHeader } from "../../components/PageHeader";
import { UseFromCodeButton } from "../../components/UseFromCode";
import { Button } from "../../components/ui";
import CypherTab from "./CypherTab";
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

/** The openCypher tab is on every server: Apache AGE where it is installed, ForgeBase's portable engine elsewhere (ADR-0034). */
const CYPHER_TAB = { to: "/graph/cypher", label: "openCypher", end: false };

function Section() {
  const nav = useNavigate();
  const { pathname } = useLocation();
  // On the Edges tab the header's action makes an edge; everywhere else it makes a node.
  const onEdges = /\/graph\/edges\/?$/.test(pathname);
  return (
    <div>
      <PageHeader
        studio="graph"
        secondary={<UseFromCodeButton kind="graph" name="" />}
        primary={onEdges
          ? <Button tone="primary" onClick={() => nav("/graph/edges?new=1")}>New edge</Button>
          : <Button tone="primary" onClick={() => nav("/graph/nodes?new=1")}>New node</Button>}
      />
      <RouteTabs label="Graph sections" tabs={[...TABS, CYPHER_TAB]} className="mt-4" />
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
        <Route path="cypher" element={<CypherTab />} />
        <Route path="*" element={<EmptyState compact title="No such page in Graph" why="The link may be old or mistyped. Pick a section above." />} />
      </Route>
    </Routes>
  );
}
