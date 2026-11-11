import { useState } from "react";
import { Badge } from "panelui-web/badge";
import { Input } from "panelui-web/input";
import { Switch } from "panelui-web/switch";
import { Textarea } from "panelui-web/textarea";
import type { FeatureSetting, SettingSource, SettingValue } from "../../api";
import { Sel } from "../../components/controls";
import { Hint, SOURCE_TEXT } from "./Hint";
import { readOnlyReason } from "./settingUtil";

const SOURCE_VARIANT: Record<SettingSource, "outline" | "info" | "warning"> = { default: "outline", file: "info", env: "warning", flag: "warning" };

/** Where a value comes from, with a tooltip that says what that means. */
export function SourceBadge({ source }: { source: SettingSource }) {
  return (
    <Hint text={SOURCE_TEXT[source]} label={`Source: ${source}. What does this mean?`}>
      <Badge variant={SOURCE_VARIANT[source]} size="sm" data-source={source}>
        {source}
      </Badge>
    </Hint>
  );
}

const listText = (v: SettingValue) => (Array.isArray(v) ? v.join("\n") : "");

/**
 * One setting as a typed PanelUI field with its source badge, help, restart hint and inline error.
 * Secrets and env/flag settings show text only. `onChange(value, error)` reports the parsed value, or a message when the text is not valid for the type.
 */
export default function SettingField({ setting: s, value, error, onChange }: {
  setting: FeatureSetting;
  value: SettingValue;
  error?: string;
  onChange: (value: SettingValue, clientError?: string) => void;
}) {
  const id = `setting-${s.key}`;
  const reason = readOnlyReason(s);
  const locked = reason !== null || !s.editable;
  const [text, setText] = useState(() => (s.type === "list" ? listText(value) : s.type === "int" ? String(value ?? "") : String(value ?? "")));
  const descId = `${id}-help`;
  const errId = `${id}-error`;

  const describedBy = [descId, error ? errId : undefined, reason ? `${id}-why` : undefined].filter(Boolean).join(" ");
  let control;
  if (s.secret) {
    control = null;
  } else if (s.type === "map") {
    control = <MapValue id={id} label={s.label} value={value} describedBy={describedBy} />;
  } else if (s.type === "bool") {
    control = <Switch label={s.label} checked={value === true} disabled={locked} onCheckedChange={(c) => onChange(c)} />;
  } else if (s.type === "enum") {
    control = <Sel id={id} aria-label={s.label} size="md" className="w-full" value={String(value ?? "")} disabled={locked} options={s.options ?? []} onChange={(v) => onChange(v)} />;
  } else if (s.type === "list") {
    control = (
      <Textarea
        id={id} aria-label={s.label} aria-describedby={describedBy} aria-invalid={error ? true : undefined} rows={3} className="w-full font-mono" disabled={locked} value={text}
        onChange={(e) => {
          setText(e.target.value);
          onChange(e.target.value.split("\n").map((x) => x.trim()).filter(Boolean));
        }}
      />
    );
  } else {
    control = (
      <div className="flex items-center gap-2">
        <Input
          id={id} aria-label={s.label} aria-describedby={describedBy} aria-invalid={error ? true : undefined} className="w-full" disabled={locked} value={text}
          type={s.type === "url" ? "url" : "text"} inputMode={s.type === "int" ? "numeric" : undefined}
          onChange={(e) => {
            const t = e.target.value;
            setText(t);
            if (s.type !== "int") return onChange(t);
            if (/^-?\d+$/.test(t.trim())) onChange(Number(t.trim()));
            else onChange(value, "Enter a whole number");
          }}
        />
        {s.unit && <span className="shrink-0 text-xs text-muted-foreground">{s.unit}</span>}
      </div>
    );
  }

  return (
    <div className="space-y-1.5 py-3 text-sm" data-setting={s.key}>
      <div className="flex flex-wrap items-center gap-2">
        <label htmlFor={s.type === "bool" || s.secret || s.type === "map" ? undefined : id} className="font-medium">
          {s.label}
        </label>
        <code className="text-xs text-muted-foreground">{s.key}</code>
        <SourceBadge source={s.source} />
        {s.secret && <Badge variant="secondary" size="sm">secret</Badge>}
        {s.restart_required && <Badge variant="outline" size="sm">restart needed</Badge>}
      </div>
      {control}
      <p id={descId} className="text-xs text-muted-foreground">
        {s.help}
      </p>
      {reason && <p id={`${id}-why`} className="text-xs text-foreground/80">Read-only: {reason}</p>}
      {error && (
        <p id={errId} role="alert" className="text-xs font-medium text-destructive">
          {s.label}: {error}
        </p>
      )}
    </div>
  );
}

/** A map setting (name and value pairs) as a read-only table. */
function MapValue({ id, label, value, describedBy }: { id: string; label: string; value: SettingValue; describedBy: string }) {
  const rows = value && typeof value === "object" && !Array.isArray(value) ? Object.entries(value) : [];
  if (rows.length === 0) return <p id={id} className="text-xs text-muted-foreground" aria-describedby={describedBy}>None set.</p>;
  return (
    <table id={id} aria-label={label} aria-describedby={describedBy} className="w-full text-left text-xs">
      <tbody>
        {rows.map(([k, v]) => (
          <tr key={k} className="border-t border-border first:border-t-0">
            <th scope="row" className="py-1 pr-3 font-mono font-normal text-muted-foreground">{k}</th>
            <td className="py-1 font-mono">{String(v)}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}
