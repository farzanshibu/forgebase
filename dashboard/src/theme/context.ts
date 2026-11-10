import { createContext, useContext } from "react";
import type { Accent, ThemeMode } from "./theme";

export interface ThemeState {
  /** What the person chose: light, dark or follow the system. */
  mode: ThemeMode;
  /** Whether dark is in effect now (system mode resolves through prefers-color-scheme). */
  dark: boolean;
  accent: Accent;
  setMode: (mode: ThemeMode) => void;
  setAccent: (accent: Accent) => void;
}

export const ThemeContext = createContext<ThemeState | null>(null);

export function useTheme(): ThemeState {
  const t = useContext(ThemeContext);
  if (!t) throw new Error("useTheme needs a <ThemeProvider>");
  return t;
}
