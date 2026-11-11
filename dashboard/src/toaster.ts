/**
 * App-wide toasts (dashboard-ux.md rule 6: a toast on every save). Works with or without a mounted Toast.Provider.
 *
 * Usage:
 *   notify.saved("Schema");                              // "Schema saved"
 *   notify.created("Bucket avatars", "/storage/b/avatars"); // "Bucket avatars created" with an Open action
 *   notify.deleted("Topic orders");
 *   notify.copied("Request id");
 *   notify.failed(error);                                // plain title and next step from errors.ts
 *   toaster.add({ title, description, type })            // anything else
 *
 * Prefer `notify` over inline "Saved." lines; keep ErrorPanel for errors the user must read and act on in place.
 */
import { createToaster } from "panelui-web/toast";
import { explain } from "./errors";

const base = createToaster();

/**
 * One toast per action: an identical toast (same title, description and type) added again within `DUPLICATE_MS`
 * returns the first one's id instead of drawing a second card. A handler that fires twice (a double click, a
 * save plus a refresh that also reports it) then shows one toast. PanelUI draws each toast's words twice in the DOM
 * (a hidden spacer and the visible text); that is one card, not two.
 */
const DUPLICATE_MS = 600;
let last: { key: string; at: number; id: ReturnType<typeof base.add> } | undefined;

export const toaster: typeof base = Object.assign(Object.create(base) as typeof base, {
  add: ((options: Parameters<typeof base.add>[0]) => {
    const key = JSON.stringify([options.title, options.description, options.type]);
    const now = Date.now();
    if (last && last.key === key && now - last.at < DUPLICATE_MS) return last.id;
    const id = base.add(options);
    last = { key, at: now, id };
    return id;
  }) as typeof base.add,
});

let navigate: ((to: string) => void) | undefined;

/** The layout registers the router's navigate so a toast's Open action stays in the app. Returns the unregister. */
export function setToastNavigator(fn: (to: string) => void): () => void {
  navigate = fn;
  return () => {
    if (navigate === fn) navigate = undefined;
  };
}

function go(to: string) {
  if (navigate) navigate(to);
  else if (typeof window !== "undefined") window.location.assign(`${import.meta.env.BASE_URL.replace(/\/$/, "")}${to}`);
}

const cap = (s: string) => (s ? s.charAt(0).toUpperCase() + s.slice(1) : s);

export const notify = {
  saved(what: string, description?: string) {
    return toaster.add({ title: `${cap(what)} saved`, description, type: "success" });
  },
  created(what: string, href?: string) {
    return toaster.add({
      title: `${cap(what)} created`,
      type: "success",
      actionProps: href ? { children: "Open", onClick: () => go(href) } : undefined,
    });
  },
  deleted(what: string) {
    return toaster.add({ title: `${cap(what)} deleted`, type: "success" });
  },
  copied(what = "Text") {
    return toaster.add({ title: `${cap(what)} copied`, description: "It is on your clipboard.", type: "success", timeout: 2500 });
  },
  failed(error: unknown) {
    const e = explain(error);
    return toaster.add({ title: e.title, description: e.requestId ? `${e.next} Request id ${e.requestId}.` : e.next, type: "destructive", timeout: 8000 });
  },
};
