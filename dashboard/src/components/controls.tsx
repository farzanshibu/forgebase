import { useId, type ReactNode } from "react";
import { Link, useLocation } from "react-router-dom";
import { Checkbox } from "panelui-web/checkbox";
import { Select } from "panelui-web/select";
import { Switch } from "panelui-web/switch";
import { Tabs } from "panelui-web/tabs";

/**
 * Form and navigation controls used by the studios, each with a value-in, value-out API (audit A-24: no native
 * checkboxes). Kept out of ui.tsx so the first-load chunk does not carry Select and Tabs.
 *
 * Usage:
 *   <Sel aria-label="Visibility" value={v} onChange={setV} options={["private", "public"]} />
 *   <Check checked={force} onChange={setForce} label="Also delete its files" />
 *   <Toggle checked={enabled} onChange={setEnabled} label="Versioning" description="Keep earlier versions of each file." />
 *   <RouteTabs label="Queues sections" tabs={[{ to: "/queues", label: "Topics", end: true }, …]} />
 *
 * Inside FormField, spread its props: {(p) => <Check {...p} checked={x} onChange={setX} label="Required" />}.
 */

export type SelectOption<T extends string | number = string> = T | { value: T; label: ReactNode };

/**
 * PanelUI Select behind a value-in, value-out API (strings, or numbers when the options carry numbers). An empty `value` that no option carries shows the placeholder.
 * Give it an `aria-label` (or an `id` that a label points at) so the trigger has a name.
 */
export function Sel<T extends string | number = string>({ value, onChange, options, placeholder, size = "sm", className, disabled, required, name, id, "aria-label": ariaLabel }: {
  value: T;
  onChange: (v: T) => void;
  options: readonly SelectOption<T>[];
  placeholder?: string;
  size?: "sm" | "md" | "lg";
  className?: string;
  disabled?: boolean;
  required?: boolean;
  name?: string;
  id?: string;
  "aria-label"?: string;
}) {
  const items = options.map((o) => (typeof o === "object" ? o : { value: o, label: String(o) }));
  const known = items.some((o) => o.value === value);
  // The Select treats "" as no value and shows the placeholder: an option such as { value: "", label: "Any type" }
  // is the placeholder by default, so the trigger reads "Any type", not blank.
  const emptyLabel = items.find((o) => o.value === "")?.label;
  return (
    <Select<T> items={items} value={known ? value : null} onValueChange={(v) => onChange((v ?? "") as T)} disabled={disabled} required={required} name={name}>
      <Select.Trigger id={id} size={size} aria-label={ariaLabel} className={className}>
        <Select.Value placeholder={placeholder ?? (typeof emptyLabel === "string" || typeof emptyLabel === "number" ? String(emptyLabel) : "")} />
      </Select.Trigger>
      <Select.Content>
        {items.map((o) => (
          <Select.Item key={String(o.value)} value={o.value} data-value={String(o.value)}>
            {o.label}
          </Select.Item>
        ))}
      </Select.Content>
    </Select>
  );
}

interface BoolControl {
  checked: boolean;
  onChange: (checked: boolean) => void;
  /** Visible words next to the control (also its accessible name). */
  label?: ReactNode;
  /** Accessible name when there are no visible words. */
  "aria-label"?: string;
  /** A quieter line under the label. */
  description?: ReactNode;
  disabled?: boolean;
  name?: string;
  id?: string;
  className?: string;
  "aria-describedby"?: string;
  "aria-invalid"?: true;
}

function BoolRow({ id, label, description, className = "", control }: { id: string; label?: ReactNode; description?: ReactNode; className?: string; control: ReactNode }) {
  if (!label && !description) return <>{control}</>;
  return (
    <span className={`inline-flex items-start gap-2 text-sm ${className}`.trim()}>
      {control}
      <span className="flex flex-col">
        {label && (
          <label htmlFor={id} className="cursor-pointer leading-5">
            {label}
          </label>
        )}
        {description && <span className="text-xs text-muted-foreground">{description}</span>}
      </span>
    </span>
  );
}

/** PanelUI Checkbox with `checked` in and `onChange(boolean)` out. */
export function Check({ checked, onChange, label, description, disabled, name, id, className, "aria-label": ariaLabel, ...aria }: BoolControl) {
  const auto = useId();
  const cid = id ?? `chk${auto.replace(/:/g, "")}`;
  return (
    <BoolRow
      id={cid}
      label={label}
      description={description}
      className={className}
      control={<Checkbox id={cid} size="sm" checked={checked} onCheckedChange={(v) => onChange(v === true)} disabled={disabled} name={name} label={ariaLabel} {...aria} className="mt-0.5" />}
    />
  );
}

/** PanelUI Switch with `checked` in and `onChange(boolean)` out, for settings that apply at once. */
export function Toggle({ checked, onChange, label, description, disabled, name, id, className, pending, "aria-label": ariaLabel, ...aria }: BoolControl & { pending?: boolean }) {
  const auto = useId();
  const cid = id ?? `tgl${auto.replace(/:/g, "")}`;
  return (
    <BoolRow
      id={cid}
      label={label}
      description={description}
      className={className}
      control={<Switch id={cid} size="sm" checked={checked} onCheckedChange={(v) => onChange(v)} disabled={disabled} name={name} pending={pending} label={ariaLabel} {...aria} />}
    />
  );
}

/** `to` may carry a query string (kept in the link, ignored when matching the location). */
export type RouteTab = { to: string; label: ReactNode; end?: boolean };

const pathOf = (to: string) => to.split(/[?#]/)[0] ?? to;

const decode = (s: string) => {
  try {
    return decodeURIComponent(s).replace(/\/+$/, "") || "/";
  } catch {
    return s.replace(/\/+$/, "") || "/";
  }
};

/** The tab whose route the location is in: exact for `end` tabs, otherwise the longest prefix. */
export function activeTab(tabs: readonly RouteTab[], pathname: string): string | null {
  const here = decode(pathname);
  let best: string | null = null;
  for (const t of tabs) {
    const to = decode(pathOf(t.to));
    const hit = t.end ? here === to : here === to || here.startsWith(`${to}/`);
    if (hit && (best === null || to.length > decode(pathOf(best)).length)) best = t.to;
  }
  return best;
}

/**
 * PanelUI tabs driven by the router: the active tab comes from the location and each tab is a router link
 * (role "tab" on an anchor), so choosing one navigates and URLs, deep links, open-in-new-tab and the back button keep working. A row wider than its space scrolls, with a fade at the end that has more.
 */
export function RouteTabs({ label, tabs, className = "" }: { label: string; tabs: readonly RouteTab[]; className?: string }) {
  const { pathname } = useLocation();
  return (
    <Tabs variant="underline" value={activeTab(tabs, pathname) ?? ""} className={className}>
      <Tabs.List aria-label={label}>
        {tabs.map((t) => (
          <Tabs.Trigger key={t.to} value={t.to} nativeButton={false} render={<Link to={t.to} />}>
            {t.label}
          </Tabs.Trigger>
        ))}
      </Tabs.List>
    </Tabs>
  );
}
