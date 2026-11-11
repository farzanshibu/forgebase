/**
 * Hint: a word or badge that explains itself on hover and focus, for the badges on feature cards ("always on",
 * "default", "file", "env", "flag"). Like HelpTip but with free text, because these explain a state, not a glossary term.
 *
 * Usage:
 *   <Hint text="Set by an environment variable. It wins over the config file."><Badge>env</Badge></Hint>
 */
import { Tooltip } from "panelui-web/tooltip";
import { useId, type ReactNode } from "react";

export function Hint({ text, children, label }: { text: string; children: ReactNode; label?: string }) {
  const id = `hint${useId().replace(/:/g, "")}`;
  return (
    <Tooltip>
      <Tooltip.Trigger type="button" aria-describedby={id} aria-label={label} className="inline-flex cursor-help items-center rounded focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring">
        {children}
      </Tooltip.Trigger>
      <Tooltip.Content variant="rich" className="max-w-xs">
        <Tooltip.Description>{text}</Tooltip.Description>
      </Tooltip.Content>
      <span id={id} hidden>
        {text}
      </span>
    </Tooltip>
  );
}

export const SOURCE_TEXT = {
  default: "The built-in value. Nothing in your config file sets it.",
  file: "Set in your config file.",
  env: "Set by an environment variable. It wins over the config file, so change it where the server starts.",
  flag: "Set by a command-line flag. It wins over the config file, so change it in the server's start command.",
} as const;

export const ALWAYS_ON_TEXT = "The server needs this to run, so it cannot be switched off.";
