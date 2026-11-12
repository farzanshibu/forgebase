/**
 * Small pieces the Database and Storage pages share. The table card, the facts strip and the icon button are the
 * shared components (components/TableCard, components/Facts, components/ui), re-exported for the pages that import
 * them from here.
 *
 * Usage:
 *   <Icon icon={PlusSignIcon} size={14} />
 */
import { HugeiconsIcon, type IconSvgElement } from "@hugeicons/react";

export { Facts } from "../../components/Facts";
export { TableCard } from "../../components/TableCard";
export type { TableCardProps } from "../../components/TableCard";
export { IconButton } from "../../components/ui";

export function Icon({ icon, size = 16, className = "" }: { icon: IconSvgElement; size?: number; className?: string }) {
  return <HugeiconsIcon icon={icon} size={size} strokeWidth={1.8} aria-hidden="true" className={`shrink-0 ${className}`.trim()} />;
}
