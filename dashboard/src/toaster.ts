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

export const toaster = createToaster();

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
