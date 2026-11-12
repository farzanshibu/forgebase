/**
 * TableCard: a list or table in a bordered card, the same in every studio. A header with the title, a count chip, one
 * sentence and the actions on the right; an optional toolbar row (search, filters); the table flush with the card
 * edges; an optional footer for a Pager. The card is a region named by its title.
 *
 * Usage:
 *   <TableCard title="Invoices" count={rows.length} description="Newest first." actions={<Button>Refresh</Button>}
 *     toolbar={<Input size="sm" aria-label="Search" />} footer={<Pager … />}>
 *     <Table framed density="compact" aria-label="Invoices">
 *       <Table.Header className={HEAD_ROW}><Table.Row><Th>Invoice</Th><Th align="end">Amount</Th></Table.Row></Table.Header>
 *       …<Table.Cell className={NUM}>…</Table.Cell>…
 *     </Table>
 *   </TableCard>
 *   <TableCard title="Keys"><CardBody><EmptyState compact … /></CardBody></TableCard>   // non-table content is padded
 *
 * Use `framed` on the Table so the first and last columns line up with the header's padding.
 */
import { Table, type TableAlign } from "panelui-web/table";
import { useId, type ReactNode } from "react";

export interface TableCardProps {
  title?: ReactNode;
  /** A quiet number after the title ("3", "25+"). */
  count?: number | string;
  /** One sentence under the title. */
  description?: ReactNode;
  /** Buttons at the right of the header. */
  actions?: ReactNode;
  /** A row under the header: search, filters. */
  toolbar?: ReactNode;
  /** A row under the table: a Pager, a hint. */
  footer?: ReactNode;
  /** Accessible name of the region; defaults to the title. */
  "aria-label"?: string;
  /** Same as `aria-label`. */
  label?: string;
  className?: string;
  children?: ReactNode;
}

export function TableCard({ title, count, description, actions, toolbar, footer, "aria-label": ariaLabel, label, className = "", children }: TableCardProps) {
  const titleId = `tc${useId().replace(/:/g, "")}`;
  const name = ariaLabel ?? label ?? (typeof title === "string" ? title : undefined);
  const hasTitle = title !== undefined && title !== null && title !== false && title !== "";
  return (
    <section
      aria-label={name}
      aria-labelledby={name === undefined && hasTitle ? titleId : undefined}
      className={`min-w-0 overflow-hidden rounded-xl border border-border bg-card ${className}`.trim()}
    >
      {(hasTitle || description || actions) && (
        <header className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2 border-b border-border px-4 py-3">
          <div className="min-w-0 flex-[1_1_16rem]">
            {hasTitle && (
              <h2 className="flex items-center gap-2 text-sm font-semibold text-foreground">
                <span id={titleId} className="min-w-0">
                  {title}
                </span>
                {count !== undefined && count !== "" && <span className="rounded-full bg-muted px-2 py-px text-xs font-medium tabular-nums text-muted-foreground">{count}</span>}
              </h2>
            )}
            {description && <p className="mt-0.5 max-w-2xl text-xs text-muted-foreground">{description}</p>}
          </div>
          {actions && <div className="flex shrink-0 flex-wrap items-center gap-2">{actions}</div>}
        </header>
      )}
      {toolbar && <div className="flex flex-wrap items-center gap-x-4 gap-y-2 border-b border-border bg-muted/30 px-4 py-2.5">{toolbar}</div>}
      {children}
      {footer && <footer className="border-t border-border px-4 py-3 [&>nav]:pt-0">{footer}</footer>}
    </section>
  );
}

/**
 * Padding for what sits in a TableCard but is not a table: an empty state, a skeleton, an error, a form. It hides
 * itself when what it holds renders nothing, so `<CardBody><ErrorPanel error={e} /></CardBody>` leaves no gap.
 */
export function CardBody({ children, className = "" }: { children?: ReactNode; className?: string }) {
  return <div className={`p-4 empty:hidden ${className}`.trim()}>{children}</div>;
}

/** A column heading; `align="end"` for amounts and counts. */
export function Th({ children, align, className }: { children?: ReactNode; align?: TableAlign; className?: string }) {
  return (
    <Table.Head align={align} className={className}>
      {children}
    </Table.Head>
  );
}

/** A numeric cell or heading: right aligned, figures of one width. */
export const NUM = "text-right tabular-nums";

/** The header row of a table in a TableCard (on `Table.Header`). */
export const HEAD_ROW = "bg-muted/40";
