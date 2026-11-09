import { NavLink, Route, Routes } from "react-router-dom";
import FunctionList from "./FunctionList";
import FunctionView from "./FunctionView";

export default function FunctionsPage() {
  return (
    <div>
      <h1 className="text-2xl font-semibold tracking-tight">Functions Studio</h1>
      <nav aria-label="Functions sections" className="mt-4 flex gap-1 border-b border-zinc-800">
        <NavLink to="/functions" end className={({ isActive }) => `-mb-px border-b-2 px-3 py-2 text-sm ${isActive ? "border-amber-400 text-white" : "border-transparent text-zinc-400 hover:text-zinc-100"}`}>
          Functions
        </NavLink>
      </nav>
      <div className="mt-6">
        <Routes>
          <Route index element={<FunctionList />} />
          <Route path="f/:name/*" element={<FunctionView />} />
          <Route path="*" element={<p className="text-zinc-400">No such page in Functions Studio.</p>} />
        </Routes>
      </div>
    </div>
  );
}
