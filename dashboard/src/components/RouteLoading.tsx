import { Spinner } from "panelui-web/spinner";

/** Themed fallback shown while a lazily loaded studio chunk is fetched. */
export default function RouteLoading() {
  return (
    <p role="status" className="flex items-center gap-2 p-8 text-sm text-muted-foreground">
      <Spinner aria-hidden="true" role="presentation" /> Loading…
    </p>
  );
}
