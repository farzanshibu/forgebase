/**
 * A router link drawn as a button (PanelUI Button with `render`): an action that goes to another screen is a link,
 * so it opens in a new tab and reads as a link to a screen reader.
 *
 * Usage:
 *   <LinkButton to="/database/new" tone="primary">New collection</LinkButton>
 */
import type { ReactNode } from "react";
import { Link } from "react-router-dom";
import { Button as PanelButton } from "panelui-web/button";

export function LinkButton({ to, tone = "default", children, className = "", "aria-label": label }: { to: string; tone?: "default" | "primary" | "ghost"; children: ReactNode; className?: string; "aria-label"?: string }) {
  const variant = tone === "primary" ? "default" : tone === "ghost" ? "ghost" : "outline";
  return (
    <PanelButton size="sm" variant={variant} nativeButton={false} render={<Link to={to} aria-label={label} />} className={className}>
      {children}
    </PanelButton>
  );
}

export default LinkButton;
