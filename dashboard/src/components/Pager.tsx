/**
 * Pager: one way to page through a list (dashboard-ux.md rule 5, audit A-19).
 *   - Tables: Previous and Next with "1–25 of 130" (`mode="pages"`).
 *   - Streams (logs, objects, messages, keys): "Load more" with how many are shown (`mode="more"`, pair with
 *     `useCursorList` from hooks.ts).
 *
 * Usage:
 *   <Pager page={page} perPage={25} total={data.total_items} onPage={setPage} label="Records" />
 *   <Pager mode="more" shown={list.items.length} hasMore={list.hasMore} loading={list.loadingMore} onMore={list.loadMore} noun="objects" />
 *
 * `page` is 1-based. When the total is unknown, pass `hasNext` instead and the count reads "Page 3".
 */
import { Button } from "./Button";

export type PagerProps =
  | {
      mode?: "pages";
      page: number;
      perPage: number;
      /** Total items, when the server counts them. */
      total?: number;
      /** When `total` is unknown: is there a next page? */
      hasNext?: boolean;
      onPage: (page: number) => void;
      loading?: boolean;
      /** Names the navigation landmark: "Records pages". */
      label?: string;
    }
  | {
      mode: "more";
      shown: number;
      hasMore: boolean;
      onMore: () => void;
      loading?: boolean;
      /** Plural noun for the count: "12 objects shown". */
      noun?: string;
      label?: string;
    };

export function Pager(props: PagerProps) {
  if (props.mode === "more") {
    const { shown, hasMore, onMore, loading, noun = "items", label = "More results" } = props;
    return (
      <nav aria-label={label} className="flex flex-wrap items-center justify-between gap-2 pt-3 text-sm text-muted-foreground">
        <span aria-live="polite">
          {shown} {noun} shown{hasMore ? "" : ", that is all"}
        </span>
        {hasMore && (
          <Button type="button" onClick={onMore} disabled={loading}>
            {loading ? "Loading…" : "Load more"}
          </Button>
        )}
      </nav>
    );
  }
  const { page, perPage, total, hasNext, onPage, loading, label = "Pages" } = props;
  const pages = total !== undefined ? Math.max(1, Math.ceil(total / perPage)) : undefined;
  const from = total === 0 ? 0 : (page - 1) * perPage + 1;
  const to = total !== undefined ? Math.min(page * perPage, total) : page * perPage;
  const canNext = pages !== undefined ? page < pages : !!hasNext;
  return (
    <nav aria-label={label} className="flex flex-wrap items-center justify-between gap-2 pt-3 text-sm text-muted-foreground">
      <span aria-live="polite">{total !== undefined ? `${from}–${to} of ${total}` : `Page ${page}`}</span>
      <span className="flex gap-2">
        <Button type="button" onClick={() => onPage(page - 1)} disabled={loading || page <= 1}>
          Previous
        </Button>
        <Button type="button" onClick={() => onPage(page + 1)} disabled={loading || !canNext}>
          Next
        </Button>
      </span>
    </nav>
  );
}

export default Pager;
