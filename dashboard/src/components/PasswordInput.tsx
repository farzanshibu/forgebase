/**
 * PasswordInput: a password field with a show / hide button inside it, and an optional strength hint for a new password.
 *
 * Usage:
 *   <FormField label="Password">{(p) => <PasswordInput {...p} value={pw} onChange={(e) => setPw(e.target.value)} autoComplete="current-password" />}</FormField>
 *   <PasswordInput {...p} value={pw} onChange={…} autoComplete="new-password" />
 *   <PasswordStrength value={pw} min={10} />
 */
import { ViewIcon, ViewOffSlashIcon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { Input } from "panelui-web/input";
import { useState, type ComponentProps } from "react";
import { passwordScore } from "./passwordScore";

export function PasswordInput({ className = "", ...p }: Omit<ComponentProps<typeof Input>, "type">) {
  const [show, setShow] = useState(false);
  return (
    <div className="relative">
      <Input {...p} type={show ? "text" : "password"} className={`w-full pe-10 ${className}`.trim()} />
      <button
        type="button"
        onClick={() => setShow((s) => !s)}
        aria-label={show ? "Hide password" : "Show password"}
        aria-pressed={show}
        title={show ? "Hide password" : "Show password"}
        className="absolute inset-y-0 end-0 flex w-10 cursor-pointer items-center justify-center rounded-e-lg text-muted-foreground outline-none transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
      >
        <HugeiconsIcon icon={show ? ViewOffSlashIcon : ViewIcon} size={17} strokeWidth={1.8} aria-hidden="true" />
      </button>
    </div>
  );
}

const LEVELS = [
  { label: "", bar: "bg-border", text: "text-muted-foreground" },
  { label: "Too short", bar: "bg-destructive", text: "text-destructive" },
  { label: "Fair", bar: "bg-warning", text: "text-warning" },
  { label: "Good", bar: "bg-success", text: "text-success" },
  { label: "Strong", bar: "bg-success", text: "text-success" },
] as const;

/** Four bars and a word under a new-password field. Says nothing until the user types. */
export function PasswordStrength({ value, min, id }: { value: string; min: number; id?: string }) {
  const score = passwordScore(value, min);
  const level = LEVELS[score]!;
  return (
    <div id={id} className="flex items-center gap-2" aria-live="polite">
      <div className="flex flex-1 gap-1" aria-hidden="true">
        {[1, 2, 3, 4].map((i) => (
          <span key={i} className={`h-1 flex-1 rounded-full transition-colors duration-200 ${i <= score ? level.bar : "bg-border"}`} />
        ))}
      </div>
      <span className={`w-16 text-end text-xs font-medium ${level.text}`}>{score > 0 ? level.label : ""}</span>
    </div>
  );
}
