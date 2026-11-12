/**
 * Light / dark / system theme and the accent colour, kept per browser in localStorage.
 * index.html runs the same logic inline before first paint (no flash); this module keeps
 * it in step once React is running. Keep the keys and ids in step with index.html.
 */

export type ThemeMode = "light" | "dark" | "system";

export const MODES: { id: ThemeMode; label: string }[] = [
  { id: "light", label: "Light" },
  { id: "dark", label: "Dark" },
  { id: "system", label: "System" },
];

/** Accent presets; the colours live in src/index.css under `[data-accent="<id>"]`. */
export const ACCENTS = [
  // The id stays "amber" so a stored choice keeps working; the colour is the forge orange.
  { id: "amber", label: "Orange" },
  { id: "indigo", label: "Indigo" },
  { id: "emerald", label: "Emerald" },
  { id: "rose", label: "Rose" },
  { id: "sky", label: "Sky" },
  { id: "violet", label: "Violet" },
] as const;

export type Accent = (typeof ACCENTS)[number]["id"];

export const MODE_KEY = "forgebase.admin.theme";
export const ACCENT_KEY = "forgebase.admin.accent";
export const DEFAULT_MODE: ThemeMode = "system";
export const DEFAULT_ACCENT: Accent = "amber";

const DARK_QUERY = "(prefers-color-scheme: dark)";

export function isMode(v: unknown): v is ThemeMode {
  return v === "light" || v === "dark" || v === "system";
}

export function isAccent(v: unknown): v is Accent {
  return ACCENTS.some((a) => a.id === v);
}

function read(key: string): string | null {
  try {
    return window.localStorage.getItem(key);
  } catch {
    return null;
  }
}

function write(key: string, value: string) {
  try {
    window.localStorage.setItem(key, value);
  } catch {
    // Private mode or blocked storage: the choice lasts for this page only.
  }
}

export function storedMode(): ThemeMode {
  const v = read(MODE_KEY);
  return isMode(v) ? v : DEFAULT_MODE;
}

export function storedAccent(): Accent {
  const v = read(ACCENT_KEY);
  return isAccent(v) ? v : DEFAULT_ACCENT;
}

export function saveMode(mode: ThemeMode) {
  write(MODE_KEY, mode);
}

export function saveAccent(accent: Accent) {
  write(ACCENT_KEY, accent);
}

export function systemPrefersDark(): boolean {
  try {
    return typeof window.matchMedia === "function" && window.matchMedia(DARK_QUERY).matches;
  } catch {
    return false;
  }
}

/** Calls `onChange` when the operating system switches between light and dark. */
export function watchSystem(onChange: () => void): () => void {
  if (typeof window.matchMedia !== "function") return () => {};
  const q = window.matchMedia(DARK_QUERY);
  q.addEventListener?.("change", onChange);
  return () => q.removeEventListener?.("change", onChange);
}

export function resolveDark(mode: ThemeMode): boolean {
  return mode === "dark" || (mode === "system" && systemPrefersDark());
}

/** Sets the `dark` class, `data-accent` and `color-scheme` on <html>. */
export function applyTheme(mode: ThemeMode, accent: Accent, root: HTMLElement = document.documentElement) {
  const dark = resolveDark(mode);
  root.classList.toggle("dark", dark);
  root.dataset.accent = accent;
  root.dataset.theme = mode;
  root.style.colorScheme = dark ? "dark" : "light";
}
