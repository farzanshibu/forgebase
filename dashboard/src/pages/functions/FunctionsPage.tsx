import { Link, Route, Routes } from "react-router-dom";
import { PageHeader } from "../../components/PageHeader";
import FunctionList from "./FunctionList";
import NewFunction from "./NewFunction";
import FunctionView from "./FunctionView";

/** The header of the list ("New function", "Upload .wasm") lives in FunctionList; a detail page draws its own. */
export default function FunctionsPage() {
  return (
    <Routes>
      <Route index element={<FunctionList />} />
      <Route path="new" element={<NewFunction />} />
      <Route path="f/:name/*" element={<FunctionView />} />
      <Route path="*" element={<div className="space-y-4"><PageHeader studio="functions" /><p className="text-muted-foreground">No such page in Functions. <Link className="text-primary hover:underline" to="/functions">Back to functions</Link></p></div>} />
    </Routes>
  );
}
