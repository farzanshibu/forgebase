import { Route, Routes } from "react-router-dom";
import { RouteTabs } from "../panel/RouteTabs";
import JobDetail from "./JobDetail";
import JobList from "./JobList";
import Schedules from "./Schedules";
import WorkflowDetail from "./WorkflowDetail";
import WorkflowList from "./WorkflowList";

export default function JobsPage() {
  return (
    <div>
      <h1 className="text-2xl font-semibold tracking-tight">Jobs</h1>
      <RouteTabs label="Jobs sections" tabs={[{ to: "/jobs", label: "Queues and jobs", end: true }, { to: "/jobs/workflows", label: "Workflows" }, { to: "/jobs/schedules", label: "Schedules" }]} className="mt-4" />
      <div className="mt-6">
        <Routes>
          <Route index element={<JobList />} />
          <Route path="schedules" element={<Schedules />} />
          <Route path="workflows" element={<WorkflowList />} />
          <Route path="w/:id" element={<WorkflowDetail />} />
          <Route path="j/:id" element={<JobDetail />} />
          <Route path="*" element={<p className="text-muted-foreground">No such page in Jobs.</p>} />
        </Routes>
      </div>
    </div>
  );
}
