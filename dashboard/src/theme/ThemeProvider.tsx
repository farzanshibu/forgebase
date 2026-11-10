import { useEffect, useLayoutEffect, useMemo, useState, type ReactNode } from "react";
import { ThemeContext, type ThemeState } from "./context";
import { applyTheme, resolveDark, saveAccent, saveMode, storedAccent, storedMode, watchSystem, type Accent, type ThemeMode } from "./theme";

/** Holds the theme mode and accent, persists them and keeps <html> in step. */
export default function ThemeProvider({ children }: { children: ReactNode }) {
  const [mode, setModeState] = useState<ThemeMode>(storedMode);
  const [accent, setAccentState] = useState<Accent>(storedAccent);
  const [dark, setDark] = useState(() => resolveDark(mode));

  useLayoutEffect(() => {
    applyTheme(mode, accent);
  }, [mode, accent, dark]);

  useEffect(() => {
    if (mode !== "system") return;
    return watchSystem(() => setDark(resolveDark("system")));
  }, [mode]);

  const value = useMemo<ThemeState>(
    () => ({
      mode,
      dark,
      accent,
      setMode: (m) => {
        saveMode(m);
        setModeState(m);
        setDark(resolveDark(m));
      },
      setAccent: (a) => {
        saveAccent(a);
        setAccentState(a);
      },
    }),
    [mode, dark, accent],
  );

  return <ThemeContext.Provider value={value}>{children}</ThemeContext.Provider>;
}
