import { Route, Routes } from "react-router-dom";
import { RouteTabs } from "../../components/controls";
import FunctionList from "./FunctionList";
import FunctionView from "./FunctionView";

export default function FunctionsPage() {
  return (
    <div>
      <h1 className="text-2xl font-semibold tracking-tight">Functions Studio</h1>
      <RouteTabs label="Functions sections" tabs={[{ to: "/functions", label: "Functions", end: true }]} className="mt-4" />
      <div className="mt-6">
        <Routes>
          <Route index element={<FunctionList />} />
          <Route path="f/:name/*" element={<FunctionView />} />
          <Route path="*" element={<p className="text-muted-foreground">No such page in Functions Studio.</p>} />
        </Routes>
      </div>
    </div>
  );
}
