import { createContext } from "react";

/**
 * Where SaveBar draws inside the app shell: an element under the scrolling page (Layout owns it). Null outside the
 * shell (first-run setup, a component test), where SaveBar falls back to a sticky bar at the end of its form.
 * A plain .ts file, so SaveBar.tsx stays fast-refresh safe.
 */
export const SaveBarSlotContext = createContext<HTMLElement | null>(null);
