import { useState } from "react";
import { Input } from "panelui-web/input";
import { createSilence, MATCHER_OPS } from "../../api";
import type { LabelMatcher, MatcherOp } from "../../api";
import { CreateSheet } from "../../components/CreateSheet";
import { FormField } from "../../components/FormField";
import { Sel } from "../../components/controls";
import { Button } from "../../components/ui";
import { isDuration } from "./util";

const BLANK: LabelMatcher = { label: "", op: "=", value: "" };

/**
 * New silence. Mount it when it should open (`{silencing && <SilenceSheet … />}`): `initial` prefills the matchers
 * and the comment, as when silencing an alert from the Alerts list.
 */
export default function SilenceSheet({ open, onOpenChange, initial, onCreated }: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  initial?: { matchers: LabelMatcher[]; comment?: string };
  onCreated: () => void;
}) {
  const [matchers, setMatchers] = useState<LabelMatcher[]>(initial?.matchers.length ? initial.matchers : [{ ...BLANK }]);
  const [duration, setDuration] = useState("2h");
  const [comment, setComment] = useState(initial?.comment ?? "");

  const set = (i: number, patch: Partial<LabelMatcher>) => setMatchers((m) => m.map((x, j) => (j === i ? { ...x, ...patch } : x)));
  const complete = matchers.filter((m) => m.label.trim() && m.value.trim());
  const durationError = isDuration(duration) ? undefined : "Use a length such as 30m, 2h or 1d.";
  const matcherError = complete.length === 0 ? "Add at least one matcher with a label and a value." : undefined;

  return (
    <CreateSheet
      open={open}
      onOpenChange={onOpenChange}
      title="New silence"
      description="Mutes the notifications of alerts whose labels match every matcher. They still fire and stay listed."
      submitLabel="Create silence"
      canSubmit={!durationError && !matcherError}
      onSubmit={async () => {
        await createSilence({ matchers: complete.map((m) => ({ ...m, label: m.label.trim(), value: m.value.trim() })), duration: duration.trim(), comment: comment.trim() });
        onCreated();
        return { what: "Silence" };
      }}
    >
      <fieldset className="space-y-2">
        <legend className="text-sm font-medium">Matchers <span className="font-normal text-muted-foreground">(all must match)</span></legend>
        {matchers.map((m, i) => (
          <div key={i} className="grid grid-cols-[1fr_5rem_1fr_auto] items-center gap-2">
            <Input size="sm" aria-label={`Matcher ${i + 1} label`} placeholder="label" value={m.label} onChange={(e) => set(i, { label: e.target.value })} />
            <Sel aria-label={`Matcher ${i + 1} operator`} value={m.op} options={MATCHER_OPS} onChange={(op: MatcherOp) => set(i, { op })} />
            <Input size="sm" aria-label={`Matcher ${i + 1} value`} placeholder="value" value={m.value} onChange={(e) => set(i, { value: e.target.value })} />
            <Button aria-label={`Remove matcher ${i + 1}`} disabled={matchers.length === 1} onClick={() => setMatchers((x) => x.filter((_, j) => j !== i))}>Remove</Button>
          </div>
        ))}
        <p className="text-xs text-muted-foreground">= and != compare exactly; =~ and !~ match an anchored regular expression.</p>
        {matcherError && <p role="alert" className="text-xs text-destructive">{matcherError}</p>}
        <Button onClick={() => setMatchers((m) => [...m, { ...BLANK }])}>Add matcher</Button>
      </fieldset>
      <FormField label="Duration" help="How long from now. It can be ended earlier." error={durationError} term="silence">
        {(p) => <Input {...p} size="sm" className="w-full" value={duration} onChange={(e) => setDuration(e.target.value)} />}
      </FormField>
      <FormField label="Comment" help="Why, for whoever reads the list later.">
        {(p) => <Input {...p} size="sm" className="w-full" value={comment} onChange={(e) => setComment(e.target.value)} />}
      </FormField>
    </CreateSheet>
  );
}
