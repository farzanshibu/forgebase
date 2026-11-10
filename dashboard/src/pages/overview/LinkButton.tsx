/**
 * A router link drawn as a button: an action that goes to another screen is a link (role "link"), so it opens in a
 * new tab and reads as a link to a screen reader. It borrows PanelUI's button look (`buttonVariants`).
 *
 * Usage:
 *   <LinkButton to="/database/new" tone="primary">New collection</LinkButton>
 */
import type { ReactNode } from "react";
import { Link } from "react-router-dom";
import { buttonVariants } from "panelui-web/button";

export function LinkButton({ to, tone = "default", children, className = "" }: { to: string; tone?: "default" | "primary" | "ghost"; children: ReactNode; className?: string }) {
  const variant = tone === "primary" ? "default" : tone === "ghost" ? "ghost" : "outline";
  return (
    <Link to={to} data-slot="button" className={buttonVariants({ variant, size: "sm", className })}>
      {children}
    </Link>
  );
}

export default LinkButton;
