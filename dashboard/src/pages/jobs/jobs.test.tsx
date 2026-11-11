import { afterEach, describe, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MockApi } from "../../test/mockApi";
import { renderStudio, signedIn } from "../../test/helpers";
import { installJobs } from "../../test/fnMock";
import { pick } from "../../test/pick";
import { confirmDialog, toast } from "../../test/dataStudioMock";

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

function setup() {
  const mock = signedIn(new MockApi().install());
  return { mock, st: installJobs(mock) };
}

describe("Jobs queues and list", () => {
  it("shows queue statistics and filters jobs by status and queue", async () => {
    const { mock } = setup();
    renderStudio("/jobs");
    expect(await screen.findByLabelText("Job totals")).toHaveTextContent("1 queued · 1 running · 1 succeeded · 1 dead · 0 cancelled");
    const jobs = await screen.findByRole("table", { name: "Jobs" });
    expect(within(jobs).getAllByRole("row")).toHaveLength(5);

    await pick(screen.getByLabelText("Status"), "dead (dead-letter)");
    await waitFor(() => expect(within(screen.getByRole("table", { name: "Jobs" })).getAllByRole("row")).toHaveLength(2));
    expect(screen.getByRole("link", { name: "job_d" })).toBeInTheDocument();
    await pick(screen.getByLabelText("Status"), "all");

    await userEvent.click(screen.getByRole("button", { name: "Show jobs of mail" }));
    await waitFor(() => expect(within(screen.getByRole("table", { name: "Jobs" })).getAllByRole("row")).toHaveLength(2));
    expect(screen.getByRole("link", { name: "job_r" })).toBeInTheDocument();
    expect(mock.calls).toContain("get /api/v1/jobs/stats");
    expect(mock.problems).toEqual([]);
  });

  it("says when no job matches", async () => {
    setup();
    renderStudio("/jobs");
    await screen.findByRole("table", { name: "Jobs" });
    await userEvent.type(screen.getByLabelText("Kind"), "nope");
    expect(await screen.findByRole("region", { name: "No jobs match" })).toBeInTheDocument();
  });

  it("pages through jobs", async () => {
    const { st } = setup();
    st.jobs = Array.from({ length: 30 }, (_, i) => ({ ...st.jobs[0]!, id: `j${String(i).padStart(2, "0")}` }));
    renderStudio("/jobs");
    expect(await screen.findByText("1–25 of 30")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Next" }));
    expect(await screen.findByText("26–30 of 30")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Next" })).toBeDisabled();
  });
});

describe("Jobs detail", () => {
  it("shows a dead job with its error and history, and retries it", async () => {
    const { mock, st } = setup();
    renderStudio("/jobs/j/job_d");
    expect(await screen.findByText("Last error: smtp refused")).toBeInTheDocument();
    const history = await screen.findByRole("list", { name: "Job history" });
    expect(within(history).getAllByRole("listitem")).toHaveLength(2);
    expect(screen.getByLabelText("Payload")).toHaveTextContent('"to": "a@b.c"');
    expect(screen.queryByRole("button", { name: "Cancel job" })).not.toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Retry" }));
    await toast("Job job_d saved");
    await waitFor(() => expect(st.jobs.find((j) => j.id === "job_d")!.status).toBe("queued"));
    expect(await screen.findByRole("button", { name: "Cancel job" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Retry" })).not.toBeInTheDocument();
    expect(mock.problems).toEqual([]);
  });

  it("cancels a queued job and a running job (cooperatively)", async () => {
    const { st } = setup();
    renderStudio("/jobs/j/job_q");
    await userEvent.click(await screen.findByRole("button", { name: "Cancel job" }));
    await confirmDialog("Cancel this job?", "Cancel job");
    await waitFor(() => expect(st.jobs.find((j) => j.id === "job_q")!.status).toBe("cancelled"));
    expect(await screen.findByRole("button", { name: "Retry" })).toBeInTheDocument();
  });

  it("deletes a finished job and returns to the list; shows result of a succeeded job", async () => {
    const { st } = setup();
    renderStudio("/jobs/j/job_s");
    expect(await screen.findByLabelText("Result")).toHaveTextContent('"sent": true');
    await userEvent.click(screen.getByRole("button", { name: "Delete job" }));
    await confirmDialog("Delete job job_s?", "Delete job", "job_s");
    await toast("Job job_s saved");
    expect(await screen.findByRole("table", { name: "Jobs" })).toBeInTheDocument();
    expect(st.jobs.map((j) => j.id)).not.toContain("job_s");
  });

  it("shows the API error when the job does not exist", async () => {
    setup();
    renderStudio("/jobs/j/missing");
    expect(await screen.findByText(/no such job/)).toBeInTheDocument();
  });
});

describe("Jobs schedules", () => {
  it("lists schedules", async () => {
    setup();
    renderStudio("/jobs/schedules");
    const t = await screen.findByRole("table", { name: "Schedules" });
    expect(within(t).getByText("nightly")).toBeInTheDocument();
    expect(within(t).getByText("0 3 * * *")).toBeInTheDocument();
  });
});

describe("Jobs tenancy and workers (FB-142, FB-146)", () => {
  it("shows the tenant of each job, queue setting, schedule and workflow", async () => {
    const { st } = setup();
    st.tasks.workflows = [{ id: "wf1", name: "import", status: "running", on_failure: "fail", created_by: "admin", created_at: "2026-11-09T10:00:00Z", updated_at: "2026-11-09T10:00:00Z", tenant: "acme", steps: [] }];
    renderStudio("/jobs");
    const jobs = await screen.findByRole("table", { name: "Jobs" });
    expect(within(within(jobs).getByRole("row", { name: /job_r/ })).getByText("acme")).toBeInTheDocument();
    expect(within(within(jobs).getByRole("row", { name: /job_q/ })).getByText("Instance")).toBeInTheDocument();
    const queues = screen.getByRole("table", { name: "Queues" });
    expect(await within(within(queues).getByRole("row", { name: /mail/ })).findByText("acme")).toBeInTheDocument();
    expect(screen.getByText(/engine:read_jobs:all/)).toBeInTheDocument();
  });

  it("lists workers with their tenant and who registered them", async () => {
    setup();
    renderStudio("/jobs");
    const table = await screen.findByRole("table", { name: "Workers" });
    const w1 = within(table).getByRole("row", { name: /w1/ });
    expect(w1).toHaveTextContent("online");
    expect(within(w1).getByText("acme")).toBeInTheDocument();
    expect(w1).toHaveTextContent("svc:mailer");
    expect(within(within(table).getByRole("row", { name: /w2/ })).getByText("Instance")).toBeInTheDocument();
    expect(screen.getByText(/409 worker_id_taken/)).toBeInTheDocument();
  });

  it("shows the tenant on a job and explains reserved kinds when there are no jobs", async () => {
    const { st } = setup();
    renderStudio("/jobs/j/job_r");
    expect(await screen.findByLabelText("Job details")).toHaveTextContent("Tenantacme");
    st.jobs = [];
    renderStudio("/jobs");
    expect(await screen.findByRole("region", { name: "No jobs yet" })).toHaveTextContent(/reserved for the server/);
  });

  it("offers Use from code for the job's kind", async () => {
    setup();
    renderStudio("/jobs/j/job_d");
    await userEvent.click(await screen.findByRole("button", { name: /Use from code/ }));
    expect((await screen.findAllByText(/email\.send/)).length).toBeGreaterThan(0);
  });

  it("explains cron on an empty schedule list", async () => {
    const mock = signedIn(new MockApi().install());
    mock.on("GET", "/api/v1/jobs/schedules", () => ({ status: 200, body: { items: [] } }));
    installJobs(mock);
    renderStudio("/jobs/schedules");
    expect(await screen.findByRole("region", { name: "No schedules yet" })).toHaveTextContent("0 3 * * *");
  });
});
