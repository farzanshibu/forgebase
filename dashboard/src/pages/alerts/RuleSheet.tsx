import { useState } from "react";
import { Input } from "panelui-web/input";
import { Textarea } from "panelui-web/textarea";
import { COMPARISONS, SEVERITIES, createRule, patchRule, queryExpression } from "../../api";
import type { AlertRule, AlertSeverity, Comparison, QueryResult } from "../../api";
import { CreateSheet } from "../../components/CreateSheet";
import { FormField } from "../../components/FormField";
import { Sel } from "../../components/controls";
import { Badge } from "../../components/StatusBadge";
import { Button, ErrorView } from "../../components/ui";
import { LabelChips } from "./shared";
import { fmtValue, isDuration, kvText, parseKv, passes } from "./util";

/** The samples an expression gives right now, and which of them would be alerts under the rule's comparison. */
function TestResult({ result, op, threshold }: { result: QueryResult; op: Comparison; threshold: number | undefined }) {
  return (
    <section aria-label="Expression result" className="space-y-2 rounded-lg border border-border p-3 text-sm">
      <p className="text-xs text-muted-foreground">{result.samples.length} sample{result.samples.length === 1 ? "" : "s"} at {new Date(result.at).toLocaleTimeString()}. Nothing was changed.</p>
      {result.samples.length === 0 && <p>The expression matches no series right now. A rule on it never fires; check the metric and label names.</p>}
      <ul className="space-y-1">
        {result.samples.slice(0, 20).map((s, i) => {
          const fires = threshold !== undefined && passes(s.value, op, threshold);
          return (
            <li key={i} className="flex flex-wrap items-center gap-2">
              <span className="font-mono tabular-nums">{fmtValue(s.value)}</span>
              {Object.keys(s.labels).length > 0 ? <LabelChips labels={s.labels} label={`Sample ${i + 1} labels`} /> : <span className="text-xs text-muted-foreground">no labels</span>}
              {threshold !== undefined && <Badge tone={fires ? "bad" : "ok"}>{fires ? "would fire" : "quiet"}</Badge>}
            </li>
          );
        })}
      </ul>
      {result.samples.length > 20 && <p className="text-xs text-muted-foreground">And {result.samples.length - 20} more.</p>}
    </section>
  );
}

/** Create a custom rule, or change one (`rule` given): the expression can be tried out before it is saved. */
export default function RuleSheet({ open, onOpenChange, rule, onSaved }: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  rule?: AlertRule;
  onSaved: () => void;
}) {
  const [name, setName] = useState(rule?.name ?? "");
  const [expr, setExpr] = useState(rule?.expr ?? "");
  const [op, setOp] = useState<Comparison>(rule?.op ?? ">");
  const [threshold, setThreshold] = useState(rule ? String(rule.threshold) : "");
  const [hold, setHold] = useState(rule?.for ?? "5m");
  const [severity, setSeverity] = useState<AlertSeverity>(rule?.severity ?? "warning");
  const [labels, setLabels] = useState(kvText(rule?.labels));
  const [annotations, setAnnotations] = useState(kvText(rule?.annotations));
  const [result, setResult] = useState<QueryResult>();
  const [testError, setTestError] = useState<unknown>();
  const [testing, setTesting] = useState(false);

  const thresholdNum = threshold.trim() === "" ? undefined : Number(threshold);
  const thresholdError = thresholdNum === undefined ? "Enter the number the expression is compared with." : Number.isFinite(thresholdNum) ? undefined : "Use a number, such as 0.05.";
  const nameError = /^[A-Za-z0-9_.-]{1,128}$/.test(name) ? undefined : "Use letters, digits, _, - and . (1 to 128 characters).";
  const holdError = isDuration(hold) || hold.trim() === "0s" ? undefined : "Use a length such as 0s, 5m or 1h.";
  const lab = parseKv(labels);
  const ann = parseKv(annotations);

  async function test() {
    setTesting(true);
    setTestError(undefined);
    setResult(undefined);
    try {
      setResult(await queryExpression(expr));
    } catch (e) {
      setTestError(e);
    } finally {
      setTesting(false);
    }
  }

  return (
    <CreateSheet
      open={open}
      onOpenChange={onOpenChange}
      title={rule ? `Edit rule ${rule.name}` : "New rule"}
      description="A rule evaluates an expression over the server's metrics every interval. Each series that passes the comparison becomes an alert."
      submitLabel={rule ? "Save rule" : "Create rule"}
      toast={rule ? "saved" : "created"}
      canSubmit={!!expr.trim() && !nameError && !thresholdError && !holdError && !lab.error && !ann.error}
      onSubmit={async () => {
        const body = { expr: expr.trim(), op, threshold: thresholdNum as number, for: hold.trim(), severity, labels: lab.value, annotations: ann.value };
        if (rule) {
          await patchRule(rule.id, body);
          onSaved();
          return { what: `Rule ${rule.name}` };
        }
        await createRule({ name, ...body });
        onSaved();
        return { what: `Rule ${name}` };
      }}
    >
      <FormField label="Name" help="Becomes the alertname label." error={rule ? undefined : name ? nameError : undefined} required>
        {(p) => <Input {...p} size="sm" className="w-full" disabled={!!rule} value={name} onChange={(e) => setName(e.target.value)} />}
      </FormField>
      <FormField label="Expression" help="A PromQL subset over the server's metrics, such as sum(rate(forgebase_http_requests_total{status=~&quot;5..&quot;}[5m])). The comparison is set below, not in the expression." required>
        {(p) => <Textarea {...p} size="sm" autoResize={false} spellCheck={false} className="h-24 w-full font-mono text-xs" value={expr} onChange={(e) => { setExpr(e.target.value); setResult(undefined); setTestError(undefined); }} />}
      </FormField>
      <div className="flex items-center gap-2">
        <Button type="button" disabled={!expr.trim() || testing} onClick={() => void test()}>{testing ? "Testing…" : "Test expression"}</Button>
        <span className="text-xs text-muted-foreground">Evaluates it now and lists the series.</span>
      </div>
      <ErrorView error={testError} />
      {result && <TestResult result={result} op={op} threshold={thresholdNum !== undefined && Number.isFinite(thresholdNum) ? thresholdNum : undefined} />}
      <div className="grid grid-cols-[6rem_1fr] gap-3">
        <FormField label="Compare">{(p) => <Sel {...p} size="sm" className="w-full" value={op} options={COMPARISONS} onChange={setOp} />}</FormField>
        <FormField label="Threshold" error={threshold ? thresholdError : undefined} required>{(p) => <Input {...p} size="sm" className="w-full tabular-nums" inputMode="decimal" value={threshold} onChange={(e) => setThreshold(e.target.value)} />}</FormField>
      </div>
      <div className="grid grid-cols-2 gap-3">
        <FormField label="Hold time" help="How long the condition must hold before it fires." error={holdError}>{(p) => <Input {...p} size="sm" className="w-full" value={hold} onChange={(e) => setHold(e.target.value)} />}</FormField>
        <FormField label="Severity">{(p) => <Sel {...p} size="sm" className="w-full" value={severity} options={SEVERITIES} onChange={setSeverity} />}</FormField>
      </div>
      <FormField label="Labels" help="One name=value per line, added to its alerts." error={lab.error}>
        {(p) => <Textarea {...p} size="sm" autoResize={false} spellCheck={false} className="h-16 w-full font-mono text-xs" value={labels} onChange={(e) => setLabels(e.target.value)} />}
      </FormField>
      <FormField label="Annotations" help="One name=value per line: summary, description, runbook_url. {{ $value }} and {{ $labels.name }} are filled in." error={ann.error}>
        {(p) => <Textarea {...p} size="sm" autoResize={false} spellCheck={false} className="h-20 w-full font-mono text-xs" value={annotations} onChange={(e) => setAnnotations(e.target.value)} />}
      </FormField>
    </CreateSheet>
  );
}
