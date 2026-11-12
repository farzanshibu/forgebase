import { Cancel01Icon, MinusSignIcon, Tick02Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { useState } from "react";
import { authzCheck, listCollections, type CheckResult } from "../../api";
import { Button } from "../../components/ui";
import { useLoad } from "../../hooks";
import { ActionError, FormCard, TextField } from "./shared";
import { useRun } from "./util";

/** The evaluation pipeline, in order (ADR-0008): a request passes each step or is denied at the first that refuses. */
const STEPS = [
  ["authentication", "Who is asking"],
  ["tenant", "The organization and tenant"],
  ["permission", "Roles, overrides and denies"],
  ["entitlement", "What the plan includes"],
  ["resource", "Resource ACLs and rules"],
  ["quota", "Usage limits"],
] as const;

function Pipeline({ result }: { result: CheckResult }) {
  const failed = result.allowed ? -1 : STEPS.findIndex(([s]) => s === result.step);
  return (
    <ol aria-label="Pipeline steps" className="space-y-1.5">
      {STEPS.map(([step, what], i) => {
        const state = result.allowed || (failed >= 0 && i < failed) ? "pass" : i === failed ? "fail" : failed < 0 ? "unknown" : "skip";
        return (
          <li key={step} className={`flex items-center gap-3 rounded-lg border px-3 py-2 text-sm ${state === "fail" ? "border-destructive/40 bg-destructive/5" : "border-border"}`}>
            <span
              aria-hidden="true"
              className={`flex size-5 shrink-0 items-center justify-center rounded-full ${state === "pass" ? "bg-success/15 text-success" : state === "fail" ? "bg-destructive/15 text-destructive" : "bg-muted text-muted-foreground"}`}
            >
              <HugeiconsIcon icon={state === "pass" ? Tick02Icon : state === "fail" ? Cancel01Icon : MinusSignIcon} size={12} strokeWidth={2.5} />
            </span>
            <span className="min-w-0 flex-1">
              <span className="font-mono text-xs">{step}</span>
              <span className="ms-2 text-xs text-muted-foreground">{what}</span>
            </span>
            <span className={`text-xs ${state === "fail" ? "font-medium text-destructive" : "text-muted-foreground"}`}>
              {state === "pass" ? "Passed" : state === "fail" ? "Denied here" : state === "skip" ? "Not reached" : ""}
            </span>
          </li>
        );
      })}
    </ol>
  );
}

/** Dry-runs the evaluation pipeline (`authz/check`), optionally as another principal. */
export default function RuleTester() {
  const cols = useLoad(listCollections, "collections");
  const [permission, setPermission] = useState("data:posts:list");
  const [org, setOrg] = useState("");
  const [rtype, setRtype] = useState("");
  const [rid, setRid] = useState("");
  const [principal, setPrincipal] = useState("");
  const [result, setResult] = useState<CheckResult>();
  const [checked, setChecked] = useState("");
  const { run, error, busy } = useRun();
  const test = () =>
    void run(async () => {
      setResult(undefined);
      setChecked(permission);
      setResult(await authzCheck({
        permission,
        ...(org && { org }),
        ...(rtype && rid && { resource_type: rtype, resource_id: rid }),
        ...(principal && { principal }),
      }));
    });
  const first = cols.data?.find((c) => !c.system)?.name;
  return (
    <div className="grid items-start gap-4 lg:grid-cols-[minmax(0,1fr)_minmax(0,24rem)]">
      <FormCard
        title="Test a permission or rule"
        description="Runs the access pipeline without doing anything, and says which step allows or refuses the request. Leave the principal empty to test yourself."
      >
        <form className="space-y-4" onSubmit={(e) => { e.preventDefault(); if (permission) test(); }}>
          <div className="grid gap-3 sm:grid-cols-2">
            <TextField label="Permission" value={permission} onChange={setPermission} placeholder={first ? `data:${first}:list` : "projects:read"} help="resource:action, such as data:posts:list" mono />
            <TextField label="Principal" value={principal} onChange={setPrincipal} placeholder="user:users/abc" help="Act as this account (optional)" mono />
            <TextField label="Organization" value={org} onChange={setOrg} placeholder="acme" help="Org id or slug (optional)" mono />
            <div className="grid grid-cols-2 gap-3">
              <TextField label="Resource type" value={rtype} onChange={setRtype} placeholder="project" mono />
              <TextField label="Resource id" value={rid} onChange={setRid} placeholder="prj_…" mono />
            </div>
          </div>
          <div className="flex justify-end">
            <Button type="submit" tone="primary" disabled={!permission || busy}>{busy ? "Checking…" : "Run check"}</Button>
          </div>
        </form>
        <ActionError error={error} />
      </FormCard>
      <section aria-label="Result" className="rounded-xl border border-border bg-card">
        <div className="border-b border-border px-4 py-3">
          <h3 className="text-sm font-semibold">Result</h3>
        </div>
        <div className="space-y-3 px-4 py-4">
          {!result && <p className="text-sm text-muted-foreground">Run a check to see the answer and the step that decided it.</p>}
          {result && (
            <>
              <div role="status" aria-label="Check result" data-allowed={result.allowed}
                className={`flex items-start gap-3 rounded-lg border p-3 text-sm ${result.allowed ? "border-success/40 bg-success/10" : "border-destructive/40 bg-destructive/10"}`}>
                <span aria-hidden="true" className={`flex size-7 shrink-0 items-center justify-center rounded-full ${result.allowed ? "bg-success/20 text-success" : "bg-destructive/20 text-destructive"}`}>
                  <HugeiconsIcon icon={result.allowed ? Tick02Icon : Cancel01Icon} size={14} strokeWidth={2.5} />
                </span>
                <div className="min-w-0">
                  <p className={`font-semibold ${result.allowed ? "text-success" : "text-destructive"}`}>{result.allowed ? "Allowed" : "Denied"}</p>
                  <p className="mt-0.5 break-all font-mono text-xs text-muted-foreground">{checked}</p>
                  {!result.allowed && <p className="mt-1 text-foreground/90">Failed step: <span className="font-mono">{result.step}</span> — {result.reason}</p>}
                </div>
              </div>
              <Pipeline result={result} />
            </>
          )}
        </div>
      </section>
    </div>
  );
}
