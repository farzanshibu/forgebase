/**
 * The dashboard's button: PanelUI Button with the three tones the studios use, and a copy button.
 *
 * Usage:
 *   <Button tone="primary" onClick={save}>Save</Button>
 *   <Button tone="danger">Delete</Button>                 // outline in the destructive colour
 *   <CopyButton text={requestId} label="Copy request id" />
 *   <CopyButton text={id} label="Copy job id" iconOnly />  // a quiet icon; `label` is its name and tooltip
 *   <CopyIcon text={url} label="Copy webhook URL" />       // the same icon-only button, small ("xs")
 *
 * Import from "components/ui" in pages; this file exists so the shared components can use Button without an import cycle.
 */
import { Copy01Icon, Tick02Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
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

export interface CopyButtonProps {
  text: string;
  /** The words on the button; with `iconOnly`, its accessible name and tooltip. */
  label?: string;
  copiedLabel?: string;
  /** Only the copy icon (it turns into a tick for a moment). */
  iconOnly?: boolean;
  /** Icon size: "sm" (32 px, beside inputs) or "xs" (24 px, inline with text). Default "sm". */
  size?: "sm" | "xs";
  className?: string;
}

/** Copies `text`; its label (or icon) turns into "Copied" for two seconds. Text never goes through a toast. */
export function CopyButton({ text, label = "Copy", copiedLabel = "Copied", iconOnly = false, size = "sm", className = "" }: CopyButtonProps) {
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");
  useEffect(() => {
    if (state === "idle") return;
    const t = setTimeout(() => setState("idle"), 2000);
    return () => clearTimeout(t);
  }, [state]);
  const failed = "Copy failed: select the text instead";
  if (iconOnly) {
    return (
      <PanelButton
        type="button"
        variant="ghost"
        size="icon-sm"
        aria-label={label}
        title={label}
        className={`text-muted-foreground hover:not-disabled:text-foreground ${size === "xs" ? "size-6" : ""} ${className}`.trim()}
        onClick={(e) => {
          e.stopPropagation();
          void copyText(text).then((ok) => setState(ok ? "copied" : "failed"));
        }}
      >
        <HugeiconsIcon icon={state === "copied" ? Tick02Icon : Copy01Icon} size={size === "xs" ? 14 : 16} strokeWidth={2} aria-hidden="true" className={`${size === "xs" ? "size-3.5" : "size-4"} ${state === "copied" ? "text-success" : ""}`.trim()} />
        <span className="sr-only" aria-live="polite">
          {state === "copied" ? copiedLabel : state === "failed" ? failed : ""}
        </span>
      </PanelButton>
    );
  }
  return (
    <Button type="button" className={className} onClick={() => void copyText(text).then((ok) => setState(ok ? "copied" : "failed"))}>
      <span aria-live="polite">{state === "copied" ? copiedLabel : state === "failed" ? failed : label}</span>
    </Button>
  );
}

/** A small icon-only CopyButton for ids, URLs and commands inline with text. */
export function CopyIcon({ text, label = "Copy", size = "xs", className }: { text: string; label?: string; size?: "sm" | "xs"; className?: string }) {
  return <CopyButton text={text} label={label} iconOnly size={size} className={className} />;
}
