import { configure } from "@testing-library/react";
import { vi } from "vitest";
import "@testing-library/jest-dom/vitest";

// jsdom lacks these browser APIs; PanelUI (NavRail, Menu) and the theme code use them.
// Files that run in the node environment (live.smoke.test.ts) have no window at all.
if (typeof window !== "undefined" && typeof window.matchMedia !== "function") {
  Object.defineProperty(window, "matchMedia", {
    configurable: true,
    writable: true,
    value: (query: string): MediaQueryList => ({
      matches: false,
      media: query,
      onchange: null,
      addEventListener: () => {},
      removeEventListener: () => {},
      addListener: () => {},
      removeListener: () => {},
      dispatchEvent: () => false,
    }),
  });
}

if (typeof globalThis.ResizeObserver !== "function") {
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  };
}

// jsdom has no Web Animations API; PanelUI's scroll areas (inside Tabs) call it from a timer.
if (typeof Element !== "undefined" && typeof Element.prototype.getAnimations !== "function") {
  Element.prototype.getAnimations = () => [];
}

// Studio routes are lazy chunks: under a full parallel run (one jsdom per file) the first import of a chunk can
// take seconds, which made `findBy*` (1 s) and tests (5 s) time out at random. Give async queries and tests room;
// a real hang still fails.
configure({ asyncUtilTimeout: 8000 });
vi.setConfig({ testTimeout: 30_000, hookTimeout: 30_000 });

// jsdom has no clipboard; the copy buttons write to it.
if (typeof navigator !== "undefined" && !navigator.clipboard) {
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: async () => {}, readText: async () => "" } });
}
