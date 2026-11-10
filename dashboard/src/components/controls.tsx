import { Link, useLocation } from "react-router-dom";
import { Select } from "panelui-web/select";
import { Tabs } from "panelui-web/tabs";

// Form and navigation controls used by the studios. Kept out of ui.tsx so the first-load chunk does not carry Select and Tabs.

export type SelectOption = string | { value: string; label: string };

/**
 * PanelUI Select behind a string-in, string-out API. An empty `value` that no option carries shows the placeholder.
 * Give it an `aria-label` (or an `id` that a label points at) so the trigger has a name.
 */
export function Sel({ value, onChange, options, placeholder, size = "sm", className, disabled, required, name, id, "aria-label": ariaLabel }: {
  value: string;
  onChange: (v: string) => void;
  options: readonly SelectOption[];
  placeholder?: string;
  size?: "sm" | "md" | "lg";
  className?: string;
  disabled?: boolean;
  required?: boolean;
  name?: string;
  id?: string;
  "aria-label"?: string;
}) {
  const items = options.map((o) => (typeof o === "string" ? { value: o, label: o } : o));
  const known = items.some((o) => o.value === value);
  return (
    <Select items={items} value={known ? value : null} onValueChange={(v) => onChange(v ?? "")} disabled={disabled} required={required} name={name}>
      <Select.Trigger id={id} size={size} aria-label={ariaLabel} className={className}>
        <Select.Value placeholder={placeholder ?? ""} />
      </Select.Trigger>
      <Select.Content>
        {items.map((o) => (
          <Select.Item key={o.value} value={o.value} data-value={o.value}>
            {o.label}
          </Select.Item>
        ))}
      </Select.Content>
    </Select>
  );
}

export type RouteTab = { to: string; label: string; end?: boolean };

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
    const to = decode(t.to);
    const hit = t.end ? here === to : here === to || here.startsWith(`${to}/`);
    if (hit && (best === null || to.length > decode(best).length)) best = t.to;
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
