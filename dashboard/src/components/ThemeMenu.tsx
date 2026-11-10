import { ComputerIcon, Moon02Icon, Sun03Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { Button } from "panelui-web/button";
import { Menu } from "panelui-web/menu";
import { useContext } from "react";
import { ThemeContext, useTheme } from "../theme/context";
import ThemeProvider from "../theme/ThemeProvider";
import { ACCENTS, MODES, isAccent, isMode } from "../theme/theme";

const MODE_ICON = { light: Sun03Icon, dark: Moon02Icon, system: ComputerIcon } as const;

/** Header control: light / dark / system, and the accent colour. Both persist per browser. */
export default function ThemeMenu({ className }: { className?: string }) {
  // Shell pieces are also rendered on their own (tests, the sign-in page before App mounts one).
  const ctx = useContext(ThemeContext);
  if (!ctx)
    return (
      <ThemeProvider>
        <ThemeMenuBody className={className} />
      </ThemeProvider>
    );
  return <ThemeMenuBody className={className} />;
}

function ThemeMenuBody({ className }: { className?: string }) {
  const { mode, accent, setMode, setAccent } = useTheme();
  return (
    <Menu>
      <Menu.Trigger render={<Button variant="ghost" size="icon-sm" aria-label="Theme and accent colour" className={className} />}>
        <HugeiconsIcon icon={MODE_ICON[mode]} size={18} strokeWidth={1.8} aria-hidden="true" />
      </Menu.Trigger>
      <Menu.Content className="w-52" align="end">
        <Menu.Group>
          <Menu.GroupLabel>Theme</Menu.GroupLabel>
          <Menu.RadioGroup value={mode} onValueChange={(v: unknown) => isMode(v) && setMode(v)}>
            {MODES.map((m) => (
              <Menu.RadioItem key={m.id} value={m.id}>
                <HugeiconsIcon icon={MODE_ICON[m.id]} size={16} strokeWidth={1.8} aria-hidden="true" />
                {m.label}
              </Menu.RadioItem>
            ))}
          </Menu.RadioGroup>
        </Menu.Group>
        <Menu.Separator />
        <Menu.Group>
          <Menu.GroupLabel>Accent colour</Menu.GroupLabel>
          <Menu.RadioGroup value={accent} onValueChange={(v: unknown) => isAccent(v) && setAccent(v)}>
            {ACCENTS.map((a) => (
              <Menu.RadioItem key={a.id} value={a.id}>
                <span data-accent={a.id} aria-hidden="true" className="size-3.5 shrink-0 rounded-full bg-primary ring-1 ring-foreground/15" />
                {a.label}
              </Menu.RadioItem>
            ))}
          </Menu.RadioGroup>
        </Menu.Group>
      </Menu.Content>
    </Menu>
  );
}
