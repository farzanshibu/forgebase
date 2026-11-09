import { NavLink, Route, Routes } from "react-router-dom";
import JobDetail from "./JobDetail";
import JobList from "./JobList";
import Schedules from "./Schedules";
import WorkflowDetail from "./WorkflowDetail";
import WorkflowList from "./WorkflowList";

const tab = ({ isActive }: { isActive: boolean }) => `-mb-px border-b-2 px-3 py-2 text-sm ${isActive ? "border-amber-400 text-white" : "border-transparent text-zinc-400 hover:text-zinc-100"}`;

export default function JobsPage() {
  return (
    <div>
      <h1 className="text-2xl font-semibold tracking-tight">Jobs</h1>
      <nav aria-label="Jobs sections" className="mt-4 flex gap-1 border-b border-zinc-800">
        <NavLink to="/jobs" end className={tab}>Queues and jobs</NavLink>
        <NavLink to="/jobs/workflows" className={tab}>Workflows</NavLink>
        <NavLink to="/jobs/schedules" className={tab}>Schedules</NavLink>
      </nav>
      <div className="mt-6">
        <Routes>
          <Route index element={<JobList />} />
          <Route path="schedules" element={<Schedules />} />
          <Route path="workflows" element={<WorkflowList />} />
          <Route path="w/:id" element={<WorkflowDetail />} />
          <Route path="j/:id" element={<JobDetail />} />
          <Route path="*" element={<p className="text-zinc-400">No such page in Jobs.</p>} />
        </Routes>
      </div>
    </div>
  );
}
