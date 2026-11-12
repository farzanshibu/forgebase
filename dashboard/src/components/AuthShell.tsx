/**
 * AuthShell: the frame of the pages shown before the dashboard (sign-in, first-run setup). A brand panel on the left
 * (wide screens only) says what ForgeBase is; the form sits on the right, centred, with the theme menu above it.
 *
 * Usage:
 *   <AuthShell title="Sign in" description="Welcome back.">…form…</AuthShell>
 *   <AuthShell title="Create the administrator" eyebrow={<SetupSteps current={0} />} footer={<p>…</p>}>…</AuthShell>
 */
import {
  CloudServerIcon,
  CodeIcon,
  Database01Icon,
  Fire03Icon,
  FingerPrintIcon,
  Folder02Icon,
} from "@hugeicons/core-free-icons";
import { HugeiconsIcon, type IconSvgElement } from "@hugeicons/react";
import type { ReactNode } from "react";
import ThemeMenu from "./ThemeMenu";

const POINTS: { icon: IconSvgElement; title: string; text: string }[] = [
  { icon: Database01Icon, title: "PostgreSQL built in", text: "Collections, SQL, indexes and policies, with nothing else to run." },
  { icon: FingerPrintIcon, title: "Auth and access", text: "Users, API keys, realms, roles and single sign-on." },
  { icon: Folder02Icon, title: "Files and storage", text: "Buckets with access rules and signed links." },
  { icon: CodeIcon, title: "Functions, queues and jobs", text: "Write and deploy code from the browser." },
];

/** The ForgeBase mark: the flame in an accent square. */
export function BrandMark({ size = "md" }: { size?: "sm" | "md" | "lg" }) {
  const box = size === "lg" ? "size-11 rounded-xl" : size === "sm" ? "size-8 rounded-lg" : "size-9 rounded-[10px]";
  const icon = size === "lg" ? 24 : size === "sm" ? 17 : 20;
  return (
    <span className={`flex shrink-0 items-center justify-center bg-primary text-primary-foreground shadow-sm ring-1 ring-black/5 ${box}`} aria-hidden="true">
      <HugeiconsIcon icon={Fire03Icon} size={icon} strokeWidth={2} />
    </span>
  );
}

function BrandPanel() {
  return (
    <aside
      aria-label="About ForgeBase"
      className="relative hidden overflow-hidden border-e border-border bg-card lg:flex lg:w-[44%] lg:max-w-[640px] lg:flex-col lg:justify-between lg:p-12 xl:p-14"
    >
      {/* Accent glow and a faint grid: decoration only, drawn from the theme tokens so both themes work. */}
      <div aria-hidden="true" className="pointer-events-none absolute inset-0 bg-[radial-gradient(40rem_28rem_at_0%_0%,color-mix(in_oklab,var(--primary)_16%,transparent),transparent_70%)]" />
      <div
        aria-hidden="true"
        className="pointer-events-none absolute inset-0 opacity-[0.35] [background-image:linear-gradient(to_right,var(--border)_1px,transparent_1px),linear-gradient(to_bottom,var(--border)_1px,transparent_1px)] [background-size:44px_44px] [mask-image:radial-gradient(ellipse_at_top_left,black_10%,transparent_70%)]"
      />
      <div className="relative flex items-center gap-3">
        <BrandMark />
        <span className="text-lg font-semibold tracking-tight">ForgeBase</span>
      </div>

      <div className="relative max-w-md space-y-8">
        <div className="space-y-3">
          <h2 className="text-3xl font-semibold leading-tight tracking-tight xl:text-4xl">Your whole backend, in one binary.</h2>
          <p className="text-base text-muted-foreground">Database, auth, storage, functions, queues and billing, managed from one dashboard on your own server.</p>
        </div>
        <ul className="space-y-4">
          {POINTS.map((p) => (
            <li key={p.title} className="flex gap-3">
              <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg border border-border bg-background text-primary" aria-hidden="true">
                <HugeiconsIcon icon={p.icon} size={16} strokeWidth={1.8} />
              </span>
              <span className="text-sm">
                <span className="block font-medium">{p.title}</span>
                <span className="block text-muted-foreground">{p.text}</span>
              </span>
            </li>
          ))}
        </ul>
      </div>

      <p className="relative flex items-center gap-2 text-xs text-muted-foreground">
        <HugeiconsIcon icon={CloudServerIcon} size={14} strokeWidth={1.8} aria-hidden="true" />
        Self-hosted: your data stays on this server.
      </p>
    </aside>
  );
}

export interface AuthShellProps {
  title: ReactNode;
  description?: ReactNode;
  /** Above the title, such as the setup steps. */
  eyebrow?: ReactNode;
  /** Under the form, outside the main column's card. */
  footer?: ReactNode;
  /** The form column's max width. */
  width?: "sm" | "md";
  children: ReactNode;
}

export function AuthShell({ title, description, eyebrow, footer, width = "sm", children }: AuthShellProps) {
  return (
    <main className="flex min-h-svh bg-background">
      <BrandPanel />
      <div className="relative flex min-w-0 flex-1 flex-col">
        <div aria-hidden="true" className="pointer-events-none absolute inset-0 bg-[radial-gradient(50rem_24rem_at_50%_-10%,color-mix(in_oklab,var(--primary)_10%,transparent),transparent)] lg:hidden" />
        <div className="relative flex items-center justify-between p-4 lg:justify-end">
          <span className="flex items-center gap-2.5 lg:hidden">
            <BrandMark size="sm" />
            <span className="font-semibold tracking-tight">ForgeBase</span>
          </span>
          <ThemeMenu />
        </div>
        <div className="relative flex flex-1 items-center justify-center px-4 pb-12 pt-2 sm:px-8">
          <div className={`w-full ${width === "md" ? "max-w-md" : "max-w-sm"} space-y-6`}>
            {eyebrow}
            <header className="space-y-1.5">
              <h1 className="text-2xl font-semibold tracking-tight">{title}</h1>
              {description && <p className="text-sm text-muted-foreground">{description}</p>}
            </header>
            {children}
            {footer}
          </div>
        </div>
      </div>
    </main>
  );
}

export default AuthShell;
