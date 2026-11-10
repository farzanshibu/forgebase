import type { ReactNode } from "react";
import { Select } from "panelui-web/select";

export interface PanelOption<T extends string | number> {
  value: T;
  label: ReactNode;
}

/**
 * One PanelUI select with the shape the studios use: a value, the options, a change callback.
 * `aria-label` names the trigger for screen readers (and tests); `className` sizes the trigger.
 */
export function PanelSelect<T extends string | number>({
  value,
  onChange,
  options,
  placeholder,
  disabled,
  name,
  className,
  "aria-label": ariaLabel,
}: {
  value: T;
  onChange: (v: T) => void;
  options: PanelOption<T>[];
  placeholder?: string;
  disabled?: boolean;
  name?: string;
  className?: string;
  "aria-label"?: string;
}) {
  return (
    <Select<T> value={value} onValueChange={(v) => { if (v !== null) onChange(v); }} items={options} disabled={disabled} name={name}>
      <Select.Trigger size="sm" aria-label={ariaLabel} className={className}>
        <Select.Value placeholder={placeholder} />
      </Select.Trigger>
      <Select.Content alignItemWithTrigger={false}>
        {options.map((o) => (
          <Select.Item key={String(o.value)} value={o.value}>{o.label}</Select.Item>
        ))}
      </Select.Content>
    </Select>
  );
}
