/** Small display pieces of the Storage studio. */
import { Badge } from "panelui-web/badge";
import type { Visibility } from "../../api";
import { Icon } from "../database/parts";
import { VISIBILITY_LOOK } from "./util";

/** A bucket's visibility as a badge: a lock, a group or a globe, with what it means as its tooltip. */
export function VisibilityBadge({ value }: { value: Visibility }) {
  const look = VISIBILITY_LOOK[value] ?? VISIBILITY_LOOK.private;
  return (
    <Badge variant={look.tone} size="sm" title={look.text}>
      <Icon icon={look.icon} size={12} />
      {value}
    </Badge>
  );
}
