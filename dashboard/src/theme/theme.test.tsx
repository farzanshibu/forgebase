import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import ThemeMenu from "../components/ThemeMenu";
import ThemeProvider from "./ThemeProvider";
import { ACCENT_KEY, ACCENTS, MODE_KEY, storedAccent, storedMode } from "./theme";

const root = document.documentElement;

function resetRoot() {
  root.className = "";
  root.removeAttribute("data-accent");
  root.removeAttribute("data-theme");
  root.style.colorScheme = "";
}

/** A matchMedia whose prefers-color-scheme answer can be flipped, firing change listeners. */
function fakeSystem(initialDark: boolean) {
  let dark = initialDark;
  const listeners = new Set<() => void>();
  vi.stubGlobal("matchMedia", (query: string) => ({
    get matches() {
      return query.includes("dark") ? dark : false;
    },
    media: query,
    onchange: null,
    addEventListener: (_: string, l: () => void) => listeners.add(l),
    removeEventListener: (_: string, l: () => void) => listeners.delete(l),
    addListener: () => {},
    removeListener: () => {},
    dispatchEvent: () => false,
  }));
  return {
    set(next: boolean) {
      dark = next;
      act(() => listeners.forEach((l) => l()));
    },
  };
}

function renderMenu() {
  return render(
    <ThemeProvider>
      <ThemeMenu />
    </ThemeProvider>,
  );
}

/** Opens the menu if it is closed (radio items keep it open), then picks `name`. */
async function choose(name: string) {
  const u = userEvent.setup();
  if (!screen.queryByRole("menu")) await u.click(screen.getByRole("button", { name: "Theme and accent colour" }));
  await u.click(await screen.findByRole("menuitemradio", { name }));
}

beforeEach(() => {
  localStorage.clear();
  resetRoot();
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("theme toggle", () => {
  it("switches to dark and back to light, persisting each choice", async () => {
    fakeSystem(false);
    renderMenu();
    expect(root.classList.contains("dark")).toBe(false);

    await choose("Dark");
    expect(root.classList.contains("dark")).toBe(true);
    expect(root.dataset.theme).toBe("dark");
    expect(root.style.colorScheme).toBe("dark");
    expect(localStorage.getItem(MODE_KEY)).toBe("dark");

    await choose("Light");
    expect(root.classList.contains("dark")).toBe(false);
    expect(root.style.colorScheme).toBe("light");
    expect(localStorage.getItem(MODE_KEY)).toBe("light");
  });

  it("follows the operating system in system mode, live", async () => {
    const os = fakeSystem(true);
    renderMenu();
    expect(storedMode()).toBe("system");
    expect(root.classList.contains("dark")).toBe(true);

    os.set(false);
    expect(root.classList.contains("dark")).toBe(false);
    os.set(true);
    expect(root.classList.contains("dark")).toBe(true);

    await choose("Light");
    os.set(true);
    expect(root.classList.contains("dark")).toBe(false);
  });

  it("restores the stored mode on the next visit", () => {
    fakeSystem(false);
    localStorage.setItem(MODE_KEY, "dark");
    renderMenu();
    expect(root.classList.contains("dark")).toBe(true);
  });
});

describe("accent colour", () => {
  it("applies the chosen accent and keeps it for the next visit", async () => {
    fakeSystem(false);
    const first = renderMenu();
    expect(root.dataset.accent).toBe("amber");

    await choose("Violet");
    expect(root.dataset.accent).toBe("violet");
    expect(localStorage.getItem(ACCENT_KEY)).toBe("violet");
    first.unmount();

    resetRoot();
    renderMenu();
    expect(root.dataset.accent).toBe("violet");
    await userEvent.setup().click(screen.getByRole("button", { name: "Theme and accent colour" }));
    expect(await screen.findByRole("menuitemradio", { name: "Violet" })).toHaveAttribute("aria-checked", "true");
    expect(screen.getByRole("menuitemradio", { name: "Amber" })).toHaveAttribute("aria-checked", "false");
  });

  it("offers every preset", async () => {
    fakeSystem(false);
    renderMenu();
    await userEvent.setup().click(screen.getByRole("button", { name: "Theme and accent colour" }));
    for (const a of ACCENTS) expect(await screen.findByRole("menuitemradio", { name: a.label })).toBeInTheDocument();
  });

  it("ignores unknown stored values", () => {
    localStorage.setItem(MODE_KEY, "sepia");
    localStorage.setItem(ACCENT_KEY, "chartreuse");
    expect(storedMode()).toBe("system");
    expect(storedAccent()).toBe("amber");
  });
});

describe("blocked storage", () => {
  it("falls back to defaults and still switches for this page", async () => {
    fakeSystem(false);
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new DOMException("denied", "SecurityError");
    });
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new DOMException("denied", "SecurityError");
    });
    renderMenu();
    expect(root.dataset.accent).toBe("amber");
    await choose("Dark");
    expect(root.classList.contains("dark")).toBe(true);
    await choose("Rose");
    expect(root.dataset.accent).toBe("rose");
  });
});

describe("pre-paint script in index.html", () => {
  const html = readFileSync(resolve(process.cwd(), "index.html"), "utf8");
  const script = /<script id="theme-init">([\s\S]*?)<\/script>/.exec(html)?.[1] ?? "";
  const run = () => new Function(script)();

  it("uses the same storage keys and accent ids as the app", () => {
    expect(script).toContain(`"${MODE_KEY}"`);
    expect(script).toContain(`"${ACCENT_KEY}"`);
    const ids = /\^\(([a-z|]+)\)\$/.exec(script)?.[1]?.split("|");
    expect(ids).toEqual(ACCENTS.map((a) => a.id));
  });

  it("applies the stored mode and accent before React runs", () => {
    fakeSystem(false);
    localStorage.setItem(MODE_KEY, "dark");
    localStorage.setItem(ACCENT_KEY, "sky");
    run();
    expect(root.classList.contains("dark")).toBe(true);
    expect(root.dataset.accent).toBe("sky");
    expect(root.style.colorScheme).toBe("dark");
  });

  it("resolves system mode and survives blocked storage", () => {
    fakeSystem(true);
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new DOMException("denied", "SecurityError");
    });
    run();
    expect(root.classList.contains("dark")).toBe(true);
    expect(root.dataset.accent).toBe("amber");
    expect(root.dataset.theme).toBe("system");
  });
});
