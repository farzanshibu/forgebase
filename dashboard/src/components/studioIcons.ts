/**
 * One icon per studio (keyed by its studios.ts path), shared by the nav rail and the command palette so a studio looks
 * the same wherever it is listed. `resourceIcon` gives a palette resource (Collection, Bucket…) its studio's icon.
 */
import {
  AiNetworkIcon,
  CodeIcon,
  CreditCardIcon,
  DashboardSquare01Icon,
  Database01Icon,
  FingerPrintIcon,
  Folder02Icon,
  HierarchySquare01Icon,
  IdentityCardIcon,
  Queue01Icon,
  Alert02Icon,
  Analytics01Icon,
  ServerStack01Icon,
  Settings02Icon,
  Shield01Icon,
  TaskDaily01Icon,
  ZapIcon,
} from "@hugeicons/core-free-icons";
import type { IconSvgElement } from "@hugeicons/react";

export const STUDIO_ICONS: Record<string, IconSvgElement> = {
  "": DashboardSquare01Icon,
  database: Database01Icon,
  redis: ZapIcon,
  storage: Folder02Icon,
  vector: AiNetworkIcon,
  graph: HierarchySquare01Icon,
  auth: FingerPrintIcon,
  access: Shield01Icon,
  identity: IdentityCardIcon,
  functions: CodeIcon,
  jobs: TaskDaily01Icon,
  queues: Queue01Icon,
  billing: CreditCardIcon,
  system: ServerStack01Icon,
  alerts: Alert02Icon,
  insights: Analytics01Icon,
  "system/features": Settings02Icon,
};

export function studioIcon(path: string): IconSvgElement {
  return STUDIO_ICONS[path] ?? DashboardSquare01Icon;
}

/** Palette resource kinds (paletteData.ts SOURCES) to the studio they live in. */
const RESOURCE_STUDIO: Record<string, string> = { Collection: "database", Bucket: "storage", Function: "functions", Topic: "queues", "Vector collection": "vector" };

export function resourceIcon(kind: string): IconSvgElement {
  return studioIcon(RESOURCE_STUDIO[kind] ?? "");
}
