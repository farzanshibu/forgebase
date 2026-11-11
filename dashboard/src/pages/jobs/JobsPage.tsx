import { Outlet, Route, Routes } from "react-router-dom";
import { PageHeader } from "../../components/PageHeader";
import { RouteTabs } from "../../components/controls";
import JobDetail from "./JobDetail";
import JobList from "./JobList";
import Schedules from "./Schedules";
import WorkflowDetail from "./WorkflowDetail";
import WorkflowList from "./WorkflowList";

/** The header and the tabs belong to the three list pages; a job or a workflow draws its own header. */
function Home() {
  return (
    <div>
      <PageHeader studio="jobs" />
      <RouteTabs label="Jobs sections" tabs={[{ to: "/jobs", label: "Jobs", end: true }, { to: "/jobs/workflows", label: "Workflows" }, { to: "/jobs/schedules", label: "Schedules" }]} className="mt-4" />
      <div className="mt-6"><Outlet /></div>
    </div>
  );
}

export default function JobsPage() {
  return (
    <Routes>
      <Route element={<Home />}>
        <Route index element={<JobList />} />
        <Route path="schedules" element={<Schedules />} />
        <Route path="workflows" element={<WorkflowList />} />
      </Route>
      <Route path="w/:id" element={<WorkflowDetail />} />
      <Route path="j/:id" element={<JobDetail />} />
      <Route path="*" element={<p className="text-muted-foreground">No such page in Jobs.</p>} />
    </Routes>
  );
}
