import { useState } from "react";
import { Input } from "panelui-web/input";
import { Textarea } from "panelui-web/textarea";
import { createSlo, updateSlo } from "../../api";
import type { Slo, SloInput, SloKind } from "../../api";
import { CreateSheet } from "../../components/CreateSheet";
import { FormField } from "../../components/FormField";
import { Sel, Toggle } from "../../components/controls";
import { isDuration, kvText, parseKv, parseList } from "./util";

type Scope = "all" | "prefix" | "regex";

/** Create or change an SLO. The objective is typed as a percentage (99.9) and sent as a ratio (0.999). */
export default function SloSheet({ open, onOpenChange, slo, onSaved }: { open: boolean; onOpenChange: (open: boolean) => void; slo?: Slo; onSaved: () => void }) {
  const [name, setName] = useState(slo?.name ?? "");
  const [description, setDescription] = useState(slo?.description ?? "");
  const [kind, setKind] = useState<SloKind>(slo?.kind ?? "availability");
  const [objective, setObjective] = useState(slo ? String(Math.round(slo.objective * 100000) / 1000) : "99.9");
  const [window, setWindow] = useState(slo?.window ?? "30d");
  const [scope, setScope] = useState<Scope>(slo?.route_prefix ? "prefix" : slo?.route_regex ? "regex" : "all");
  const [route, setRoute] = useState(slo?.route_prefix ?? slo?.route_regex ?? "");
  const [methods, setMethods] = useState(slo?.methods.join(", ") ?? "");
  const [threshold, setThreshold] = useState(slo?.latency_threshold_ms ? String(slo.latency_threshold_ms) : "300");
  const [alerting, setAlerting] = useState(slo?.alerting ?? true);
  const [labels, setLabels] = useState(kvText(slo?.labels));

  const obj = Number(objective);
  const objectiveError = objective.trim() !== "" && Number.isFinite(obj) && obj >= 90 && obj <= 99.999 ? undefined : "Use a percentage from 90 to 99.999, such as 99.9.";
  const thr = Number(threshold);
  const thresholdError = kind !== "latency" || (Number.isInteger(thr) && thr > 0) ? undefined : "Use a whole number of milliseconds.";
  const nameError = /^[A-Za-z0-9_.-]{1,64}$/.test(name) ? undefined : "Use letters, digits, _, - and . (1 to 64 characters).";
  const windowError = isDuration(window) ? undefined : "Use a length from 1h to 90d, such as 30d.";
  const routeError = scope !== "all" && !route.trim() ? "Enter the route." : undefined;
  const lab = parseKv(labels);

  return (
    <CreateSheet
      open={open}
      onOpenChange={onOpenChange}
      title={slo ? `Edit SLO ${slo.name}` : "New SLO"}
      description="An objective for the share of good requests over a rolling window. Counting starts when it is created."
      submitLabel={slo ? "Save SLO" : "Create SLO"}
      toast={slo ? "saved" : "created"}
      canSubmit={!nameError && !objectiveError && !thresholdError && !windowError && !routeError && !lab.error}
      onSubmit={async () => {
        const body: SloInput = {
          name, description: description.trim(), kind, objective: Math.round(obj * 1000) / 100000, window: window.trim(), methods: parseList(methods).map((x) => x.toUpperCase()),
          alerting, labels: lab.value,
          ...(scope === "prefix" ? { route_prefix: route.trim() } : scope === "regex" ? { route_regex: route.trim() } : {}),
          ...(kind === "latency" ? { latency_threshold_ms: thr } : {}),
        };
        if (slo) await updateSlo(slo.id, body);
        else await createSlo(body);
        onSaved();
        return { what: `SLO ${name}` };
      }}
    >
      <FormField label="Name" help="Becomes the slo label of its alerts." error={name ? nameError : undefined} required>{(p) => <Input {...p} size="sm" className="w-full" disabled={!!slo} value={name} onChange={(e) => setName(e.target.value)} />}</FormField>
      <FormField label="Description">{(p) => <Input {...p} size="sm" className="w-full" value={description} onChange={(e) => setDescription(e.target.value)} />}</FormField>
      <div className="grid grid-cols-2 gap-3">
        <FormField label="Kind" help={kind === "availability" ? "Good = not answered with a 5xx." : "Good = answered within the threshold (5xx are left out)."}>{(p) => <Sel {...p} size="sm" className="w-full" value={kind} options={["availability", "latency"]} onChange={setKind} />}</FormField>
        <FormField label="Objective (%)" error={objectiveError} required>{(p) => <Input {...p} size="sm" className="w-full tabular-nums" inputMode="decimal" value={objective} onChange={(e) => setObjective(e.target.value)} />}</FormField>
      </div>
      {kind === "latency" && <FormField label="Latency threshold (ms)" error={thresholdError} required>{(p) => <Input {...p} size="sm" className="w-full tabular-nums" inputMode="numeric" value={threshold} onChange={(e) => setThreshold(e.target.value)} />}</FormField>}
      <FormField label="Window" help="1h to 90d." error={windowError}>{(p) => <Input {...p} size="sm" className="w-full" value={window} onChange={(e) => setWindow(e.target.value)} />}</FormField>
      <div className="grid grid-cols-[9rem_1fr] gap-3">
        <FormField label="Routes">{(p) => <Sel {...p} size="sm" className="w-full" value={scope} options={[{ value: "all", label: "Every route" }, { value: "prefix", label: "Starting with" }, { value: "regex", label: "Matching regex" }]} onChange={setScope} />}</FormField>
        {scope !== "all" && <FormField label={scope === "prefix" ? "Route prefix" : "Route regex"} help="Matched on the route template, such as /api/v1/collections/{name}/records." error={routeError}>{(p) => <Input {...p} size="sm" className="w-full font-mono" value={route} onChange={(e) => setRoute(e.target.value)} />}</FormField>}
      </div>
      <FormField label="Methods" help="Optional, separated by commas: GET, POST. Empty counts every method.">{(p) => <Input {...p} size="sm" className="w-full" value={methods} onChange={(e) => setMethods(e.target.value)} />}</FormField>
      <FormField label="Alert labels" help="One name=value per line, added to its burn-rate alerts." error={lab.error}>{(p) => <Textarea {...p} size="sm" autoResize={false} spellCheck={false} className="h-14 w-full font-mono text-xs" value={labels} onChange={(e) => setLabels(e.target.value)} />}</FormField>
      <Toggle checked={alerting} onChange={setAlerting} label="Burn-rate alerts" description="Page when the budget burns fast, ticket when it burns steadily." />
    </CreateSheet>
  );
}
