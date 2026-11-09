import { useState } from "react";
import { authzCheck, listCollections, type CheckResult } from "../../api";
import { Button, Panel } from "../../components/ui";
import { useLoad } from "../../hooks";
import { ActionError, TextField } from "./shared";
import { useRun } from "./util";

/** Dry-runs the evaluation pipeline (`authz/check`), optionally as another principal. */
export default function RuleTester() {
  const cols = useLoad(listCollections, "collections");
  const [permission, setPermission] = useState("data:posts:list");
  const [org, setOrg] = useState("");
  const [rtype, setRtype] = useState("");
  const [rid, setRid] = useState("");
  const [principal, setPrincipal] = useState("");
  const [result, setResult] = useState<CheckResult>();
  const { run, error } = useRun();
  const test = () =>
    void run(async () => {
      setResult(undefined);
      setResult(await authzCheck({
        permission,
        ...(org && { org }),
        ...(rtype && rid && { resource_type: rtype, resource_id: rid }),
        ...(principal && { principal }),
      }));
    });
  const first = cols.data?.find((c) => !c.system)?.name;
  return (
    <div className="space-y-4">
      <Panel title="Test a permission or rule">
        <p className="mb-3 text-xs text-zinc-500">Runs the pipeline (authentication, tenant, permission, entitlement, resource, quota) without doing anything. Leave principal empty to test yourself.</p>
        <div className="grid gap-3 sm:grid-cols-2">
          <TextField label="Permission" value={permission} onChange={setPermission} placeholder={first ? `data:${first}:list` : "projects:read"} />
          <TextField label="Principal" value={principal} onChange={setPrincipal} placeholder="user:users/abc" />
          <TextField label="Organization" value={org} onChange={setOrg} />
          <div className="grid grid-cols-2 gap-3">
            <TextField label="Resource type" value={rtype} onChange={setRtype} placeholder="project" />
            <TextField label="Resource id" value={rid} onChange={setRid} />
          </div>
        </div>
        <div className="mt-3"><Button tone="primary" disabled={!permission} onClick={test}>Run check</Button></div>
      </Panel>
      <ActionError error={error} />
      {result && (
        <div role="status" aria-label="Check result" data-allowed={result.allowed}
          className={`rounded-lg border p-4 text-sm ${result.allowed ? "border-emerald-700/60 bg-emerald-500/10 text-emerald-200" : "border-red-800/60 bg-red-500/10 text-red-200"}`}>
          <p className="font-medium">{result.allowed ? "Allowed" : "Denied"}</p>
          {!result.allowed && <p className="mt-1">Failed step: <span className="font-mono">{result.step}</span> — {result.reason}</p>}
        </div>
      )}
    </div>
  );
}
