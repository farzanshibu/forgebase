import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router-dom";
import type { Job, WorkflowDetail, WorkflowStep } from "../../api";
import { MockApi } from "../../test/mockApi";
import { renderStudio, signedIn } from "../../test/helpers";
import { installJobs } from "../../test/fnMock";
import ProgressBar from "./ProgressBar";
import { layout } from "./WorkflowGraph";

const NOW = "2026-11-09T10:00:00Z";

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

function setup() {
  const mock = signedIn(new MockApi().install());
  return { mock, st: installJobs(mock) };
}

const step = (id: string, name: string, status: Job["status"], deps: string[] = [], extra: Partial<Job> = {}, role = "step"): WorkflowStep => ({
  name, role, depends_on: deps,
  job: { ...({ id, queue: "default", kind: name, payload: {}, priority: 0, status, run_at: NOW, attempts: 1, max_attempts: 3, backoff_base_ms: 1, backoff_max_ms: 2, visibility_timeout_ms: 3, cancel_requested: false, created_by: "a", created_at: NOW, updated_at: NOW } as Job), ...extra },
});

function workflow(over: Partial<WorkflowDetail> = {}): WorkflowDetail {
  return {
    id: "wf1", name: "nightly-report", status: "running", on_failure: "fail", created_by: "admin", created_at: NOW, updated_at: NOW,
    steps: [
      step("ja", "extract", "succeeded", [], { progress: { percent: 100, stage: "done", updated_at: NOW } }),
      step("jb", "transform", "running", ["ja"], { progress: { percent: 40, stage: "rows", message: "40000 of 100000", updated_at: NOW } }),
      step("jc", "validate", "running", ["ja"]),
      step("jd", "load", "queued", ["jb", "jc"], { waiting_for: 2 }),
    ],
    ...over,
  };
}

describe("ProgressBar", () => {
  it("shows percent, stage and message, clamps, and copes with no percent", () => {
    const { rerender } = render(<ProgressBar progress={{ percent: 42.4, stage: "encode", message: "frame 10", updated_at: NOW }} />);
    expect(screen.getByRole("progressbar", { name: "Task progress" })).toHaveAttribute("aria-valuenow", "42");
    expect(screen.getByText("encode")).toBeInTheDocument();
    expect(screen.getByText("frame 10")).toBeInTheDocument();
    rerender(<ProgressBar progress={{ percent: 180, updated_at: NOW }} />);
    expect(screen.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "100");
    rerender(<ProgressBar progress={{ stage: "waiting", updated_at: NOW }} />);
    expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
    expect(screen.getByText("waiting")).toBeInTheDocument();
    rerender(<ProgressBar progress={null} />);
    expect(screen.getByText("No progress reported yet.")).toBeInTheDocument();
  });
});

describe("workflow graph layout", () => {
  it("puts each step one column right of its deepest parent and draws the edges", () => {
    const g = layout(workflow().steps);
    const col = Object.fromEntries(g.nodes.map((n) => [n.label, [n.level, n.row]]));
    expect(col).toEqual({ extract: [0, 0], transform: [1, 0], validate: [1, 1], load: [2, 0] });
    expect(g.edges.map((e) => `${e.from.id}>${e.to.id}`).sort()).toEqual(["ja>jb", "ja>jc", "jb>jd", "jc>jd"]);
    expect(g.width).toBeGreaterThan(3 * 168);
    expect(layout([]).nodes).toEqual([]);
  });

  it("ignores parents outside the workflow and survives a cycle", () => {
    const g = layout([step("a", "a", "queued", ["missing"]), step("b", "b", "queued", ["c"]), step("c", "c", "queued", ["b"])]);
    expect(g.edges.map((e) => `${e.from.id}>${e.to.id}`).sort()).toEqual(["b>c", "c>b"]);
    expect(g.nodes.find((n) => n.id === "a")?.level).toBe(0);
  });
});

describe("Job detail for long tasks", () => {
  it("shows live progress, the checkpoint, attempt history and the workflow link", async () => {
    const { st } = setup();
    const j = st.jobs.find((x) => x.id === "job_r")!;
    Object.assign(j, { progress: { percent: 25, stage: "decode", message: "frame 100/400", updated_at: NOW }, has_checkpoint: true, checkpoint_at: NOW, timeout_ms: 3_600_000, deadline_at: NOW, workflow_id: "wf1", step: "extract", parent_id: "job_q" });
    st.tasks.checkpoints.job_r = { frame: 100 };
    st.tasks.attempts.job_r = [
      { attempt: 1, worker_id: "w0", started_at: NOW, finished_at: NOW, duration_ms: 1500, outcome: "lease_expired", error: "worker vanished", from_checkpoint: false },
      { attempt: 2, worker_id: "w1", started_at: NOW, duration_ms: 400, from_checkpoint: true },
    ];
    renderStudio("/jobs/j/job_r");
    expect(await screen.findByRole("progressbar", { name: "Task progress" })).toHaveAttribute("aria-valuenow", "25");
    expect(screen.getByText("frame 100/400")).toBeInTheDocument();
    expect(screen.getByText(/Timeout 3600 s/)).toBeInTheDocument();
    expect(await screen.findByLabelText("Checkpoint")).toHaveTextContent('"frame": 100');
    const attempts = await screen.findByRole("table", { name: "Attempts" });
    expect(within(attempts).getByText("worker vanished")).toBeInTheDocument();
    expect(within(attempts).getByText("from checkpoint")).toBeInTheDocument();
    expect(within(attempts).getByText("running")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "wf1" })).toHaveAttribute("href", "/jobs/w/wf1");
    expect(screen.getByRole("link", { name: "job_q" })).toHaveAttribute("href", "/jobs/j/job_q");
  });

  it("refreshes a running task by itself", async () => {
    const { st } = setup();
    const j = st.jobs.find((x) => x.id === "job_r")!;
    j.progress = { percent: 10, stage: "one", updated_at: NOW };
    renderStudio("/jobs/j/job_r");
    expect(await screen.findByRole("progressbar")).toHaveAttribute("aria-valuenow", "10");
    j.progress = { percent: 80, stage: "two", updated_at: NOW };
    await waitFor(() => expect(screen.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "80"), { timeout: 5000 });
    expect(screen.getByText("two")).toBeInTheDocument();
  }, 10_000);

  it("says a task is waiting for its parents and offers cancel", async () => {
    const { st, mock } = setup();
    Object.assign(st.jobs.find((x) => x.id === "job_q")!, { waiting_for: 2 });
    renderStudio("/jobs/j/job_q");
    expect(await screen.findByText(/Waiting for 2 parent job\(s\)/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Cancel job" }));
    await waitFor(() => expect(st.jobs.find((x) => x.id === "job_q")?.status).toBe("cancelled"));
    expect(await screen.findByRole("button", { name: "Retry" })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(st.jobs.find((x) => x.id === "job_q")?.status).toBe("queued"));
    expect(mock.problems).toEqual([]);
  });

  it("loads a result kept in object storage on demand", async () => {
    const { st } = setup();
    Object.assign(st.jobs.find((x) => x.id === "job_s")!, { result: null, result_stored: true, result_size: 2_000_000 });
    st.tasks.results.job_s = { rows: 12345 };
    renderStudio("/jobs/j/job_s");
    expect(await screen.findByLabelText("Result")).toHaveTextContent("stored outside the job record");
    await userEvent.click(screen.getByRole("button", { name: "Load stored result (2000000 bytes)" }));
    await waitFor(() => expect(screen.getByLabelText("Result")).toHaveTextContent('"rows": 12345'));
    expect(screen.queryByRole("button", { name: /Load stored result/ })).not.toBeInTheDocument();
  });

  it("hides the long-task panels for a job without them", async () => {
    setup();
    renderStudio("/jobs/j/job_d");
    await screen.findByText("smtp refused", { exact: false });
    expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Checkpoint")).not.toBeInTheDocument();
    expect(await screen.findByText("No attempts yet.")).toBeInTheDocument();
  });
});

describe("Queue settings in the jobs list", () => {
  it("shows limits and pauses and resumes a queue", async () => {
    const { st, mock } = setup();
    renderStudio("/jobs");
    const queues = await screen.findByRole("table", { name: "Queues" });
    expect(await within(queues).findByText("2 at once, 10/second")).toBeInTheDocument();
    await userEvent.click(within(queues).getByRole("button", { name: "Pause queue mail" }));
    await waitFor(() => expect(st.tasks.queues.find((q) => q.name === "mail")?.paused).toBe(true));
    expect(await within(queues).findByText("paused")).toBeInTheDocument();
    await userEvent.click(within(queues).getByRole("button", { name: "Resume queue mail" }));
    await waitFor(() => expect(st.tasks.queues.find((q) => q.name === "mail")?.paused).toBe(false));
    expect(mock.calls).toContain("post /api/v1/jobs/queues/mail/pause");
    expect(mock.calls).toContain("post /api/v1/jobs/queues/mail/resume");
  });

  it("shows a refused pause", async () => {
    const mock = signedIn(new MockApi().install());
    mock.on("POST", "/api/v1/jobs/queues/{name}/pause", () => ({ status: 403, body: { error: { code: "forbidden", message: "no", request_id: "r" } } }));
    installJobs(mock);
    renderStudio("/jobs");
    await userEvent.click(await screen.findByRole("button", { name: "Pause queue mail" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Sign-in required");
  });
});

describe("Workflows", () => {
  it("lists workflows and filters by status", async () => {
    const { st } = setup();
    st.tasks.workflows = [workflow(), workflow({ id: "wf2", name: "cleanup", status: "failed", error: "boom" })];
    renderStudio("/jobs/workflows");
    const table = await screen.findByRole("table", { name: "Workflows" });
    expect(within(table).getAllByRole("row")).toHaveLength(3);
    await userEvent.selectOptions(screen.getByLabelText("Status"), "failed");
    await waitFor(() => expect(within(screen.getByRole("table", { name: "Workflows" })).getAllByRole("row")).toHaveLength(2));
    expect(screen.getByRole("link", { name: "cleanup" })).toHaveAttribute("href", "/jobs/w/wf2");
    await userEvent.selectOptions(screen.getByLabelText("Status"), "cancelled");
    expect(await screen.findByText("No workflows match.")).toBeInTheDocument();
  });

  it("is reachable from the Jobs tabs", async () => {
    const { st } = setup();
    st.tasks.workflows = [workflow()];
    renderStudio("/jobs");
    await userEvent.click(await screen.findByRole("link", { name: "Workflows" }));
    expect(await screen.findByRole("table", { name: "Workflows" })).toBeInTheDocument();
  });

  it("draws the step graph with statuses, links each node to its job, and lists step progress", async () => {
    const { st } = setup();
    st.tasks.workflows = [workflow()];
    renderStudio("/jobs/w/wf1");
    const graph = await screen.findByRole("img", { name: "Workflow graph" });
    expect(within(graph).getByRole("link", { name: "extract: succeeded" })).toHaveAttribute("href", "/jobs/j/ja");
    expect(within(graph).getByRole("link", { name: "transform: running" })).toBeInTheDocument();
    expect(within(graph).getByRole("link", { name: "load: queued, waiting for 2" })).toBeInTheDocument();
    expect(graph.querySelectorAll("[data-edge]")).toHaveLength(4);
    expect(graph.querySelector('[data-status="running"]')).not.toBeNull();
    const steps = screen.getByRole("table", { name: "Workflow steps" });
    expect(within(steps).getAllByRole("row")).toHaveLength(5);
    expect(within(steps).getByRole("progressbar", { name: "Progress of transform" })).toHaveAttribute("aria-valuenow", "40");
    expect(within(steps).getByText("waiting for 2")).toBeInTheDocument();
    expect(screen.getByRole("status", { name: "Workflow status" })).toHaveTextContent("running");
  });

  it("cancels a running workflow and then hides the button", async () => {
    const { st, mock } = setup();
    st.tasks.workflows = [workflow()];
    renderStudio("/jobs/w/wf1");
    await userEvent.click(await screen.findByRole("button", { name: "Cancel workflow" }));
    await waitFor(() => expect(screen.getByRole("status", { name: "Workflow status" })).toHaveTextContent("cancelled"));
    expect(screen.queryByRole("button", { name: "Cancel workflow" })).not.toBeInTheDocument();
    expect(mock.calls).toContain("post /api/v1/jobs/workflows/wf1/cancel");
  });

  it("shows a failed workflow's error with a link to the failing job, and marks compensation steps", async () => {
    const { st } = setup();
    st.tasks.workflows = [workflow({ status: "compensating", on_failure: "compensate", failed_job_id: "jb", error: "disk full", steps: [step("jb", "transform", "dead", [], {}), step("jx", "undo-transform", "queued", ["jb"], {}, "compensation")] })];
    renderStudio("/jobs/w/wf1");
    expect(await screen.findByText(/disk full/)).toBeInTheDocument();
    expect(within(screen.getByText(/disk full/).closest("p")!).getByRole("link", { name: "jb" })).toHaveAttribute("href", "/jobs/j/jb");
    expect(screen.getAllByText("compensation").length).toBeGreaterThan(0);
    expect(screen.getByRole("link", { name: "undo-transform: queued" })).toBeInTheDocument();
  });

  it("reports an unknown workflow", async () => {
    setup();
    renderStudio("/jobs/w/ghost");
    expect(await screen.findByRole("alert")).toHaveTextContent("workflow_not_found");
  });

  it("renders the graph route inside a router without the studio shell", () => {
    render(<MemoryRouter><ProgressBar progress={{ percent: 5, updated_at: NOW }} /></MemoryRouter>);
    expect(screen.getByRole("progressbar")).toBeInTheDocument();
  });
});
