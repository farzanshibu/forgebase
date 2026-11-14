import { Card as PanelCard } from "panelui-web/card";
import type { ReactNode } from "react";
import { HugeiconsIcon, type IconSvgElement } from "@hugeicons/react";

export function Card({ title, icon, children }: { title: string; icon?: IconSvgElement; children: ReactNode }) {
  return (
    <PanelCard as="section" size="sm" className="rounded-xl">
      <PanelCard.Header className="pb-2">
        <PanelCard.Title as="h2" className="flex items-center gap-2 text-sm font-medium text-foreground">
          {icon && (
            <span className="flex size-6 items-center justify-center rounded-md bg-primary/10 text-primary" aria-hidden="true">
              <HugeiconsIcon icon={icon} size={14} strokeWidth={1.8} />
            </span>
          )}
          {title}
        </PanelCard.Title>
      </PanelCard.Header>
      <PanelCard.Content>{children}</PanelCard.Content>
    </PanelCard>
  );
}

/** One label and value; `mono` for versions and ids. */
export function Row({ label, value, mono = false }: { label: string; value: ReactNode; mono?: boolean }) {
  return (
    <div className="flex items-center justify-between gap-4 border-b border-border/60 py-2 text-sm last:border-b-0">
      <dt className="shrink-0 text-muted-foreground">{label}</dt>
      <dd className={`min-w-0 truncate text-right tabular-nums text-foreground ${mono ? "font-mono text-xs" : ""}`}>{value}</dd>
    </div>
  );
}
