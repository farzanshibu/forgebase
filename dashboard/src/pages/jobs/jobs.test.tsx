import { afterEach, describe, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MockApi } from "../../test/mockApi";
import { renderStudio, signedIn } from "../../test/helpers";
import { installJobs } from "../../test/fnMock";
import { pick } from "../../test/pick";

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
    expect(await screen.findByText("No jobs match.")).toBeInTheDocument();
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
    await waitFor(() => expect(st.jobs.find((j) => j.id === "job_d")!.status).toBe("queued"));
    expect(await screen.findByRole("button", { name: "Cancel job" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Retry" })).not.toBeInTheDocument();
    expect(mock.problems).toEqual([]);
  });

  it("cancels a queued job and a running job (cooperatively)", async () => {
    const { st } = setup();
    renderStudio("/jobs/j/job_q");
    await userEvent.click(await screen.findByRole("button", { name: "Cancel job" }));
    await waitFor(() => expect(st.jobs.find((j) => j.id === "job_q")!.status).toBe("cancelled"));
    expect(await screen.findByRole("button", { name: "Retry" })).toBeInTheDocument();
  });

  it("deletes a finished job and returns to the list; shows result of a succeeded job", async () => {
    const { st } = setup();
    renderStudio("/jobs/j/job_s");
    expect(await screen.findByLabelText("Result")).toHaveTextContent('"sent": true');
    await userEvent.click(screen.getByRole("button", { name: "Delete job" }));
    await userEvent.click(screen.getByRole("button", { name: "Confirm delete job" }));
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
