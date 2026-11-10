/**
 * The dashboard's button: PanelUI Button with the three tones the studios use, and a copy button.
 *
 * Usage:
 *   <Button tone="primary" onClick={save}>Save</Button>
 *   <Button tone="danger">Delete</Button>                 // outline in the destructive colour
 *   <CopyButton text={requestId} label="Copy request id" />
 *
 * Import from "components/ui" in pages; this file exists so the shared components can use Button without an import cycle.
 */
import { useEffect, useState, type ButtonHTMLAttributes } from "react";
import { Button as PanelButton } from "panelui-web/button";

export type ButtonTone = "default" | "primary" | "danger";

export function Button({ tone = "default", className = "", ...p }: ButtonHTMLAttributes<HTMLButtonElement> & { tone?: ButtonTone }) {
  // Danger stays an outline in the destructive colour: a filled destructive button needs white text,
  // which fails AA on the light red the dark theme uses.
  const danger = tone === "danger" ? "border-destructive/50 text-destructive hover:not-disabled:bg-destructive/10 hover:not-disabled:text-destructive" : "";
  return <PanelButton size="sm" variant={tone === "primary" ? "default" : "outline"} {...p} className={`${danger} ${className}`.trim()} />;
}

/** Copy to the clipboard; false when the browser refused. */
async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

/** Copies `text`; its label turns into "Copied" for two seconds. Text never goes through a toast. */
export function CopyButton({ text, label = "Copy", copiedLabel = "Copied", className = "" }: { text: string; label?: string; copiedLabel?: string; className?: string }) {
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");
  useEffect(() => {
    if (state === "idle") return;
    const t = setTimeout(() => setState("idle"), 2000);
    return () => clearTimeout(t);
  }, [state]);
  return (
    <Button type="button" className={className} onClick={() => void copyText(text).then((ok) => setState(ok ? "copied" : "failed"))}>
      <span aria-live="polite">{state === "copied" ? copiedLabel : state === "failed" ? "Copy failed: select the text instead" : label}</span>
    </Button>
  );
}
