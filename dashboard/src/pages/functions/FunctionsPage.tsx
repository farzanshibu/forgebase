import { Link, Route, Routes } from "react-router-dom";
import { EmptyState } from "../../components/EmptyState";
import { PageHeader } from "../../components/PageHeader";
import FunctionList from "./FunctionList";
import NewFunction from "./NewFunction";
import FunctionView from "./FunctionView";
import RequestSearch from "./RequestSearch";

/** The header of the list ("New function", "Upload .wasm") lives in FunctionList; a detail page draws its own. */
export default function FunctionsPage() {
  return (
    <Routes>
      <Route index element={<FunctionList />} />
      <Route path="new" element={<NewFunction />} />
      <Route path="logs" element={<RequestSearch />} />
      <Route path="f/:name/*" element={<FunctionView />} />
      <Route path="*" element={<div className="space-y-6"><PageHeader studio="functions" /><EmptyState compact title="No such page in Functions" why="The link may be old or mistyped." action={<Link className="text-sm font-medium text-primary hover:underline" to="/functions">See all functions</Link>} /></div>} />
    </Routes>
  );
}
