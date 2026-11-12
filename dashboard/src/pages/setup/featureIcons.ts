/**
 * One icon per server feature (ids from crates/forgebase-core/src/features.rs). The studios in the nav rail use the
 * same icons, so a feature on the map and its studio in the rail look alike.
 */
import {
  AiNetworkIcon,
  CodeIcon,
  Coins01Icon,
  CommandLineIcon,
  CreditCardIcon,
  Database01Icon,
  DatabaseRestoreIcon,
  FingerPrintIcon,
  Folder02Icon,
  HierarchySquare01Icon,
  IdentityCardIcon,
  PaintBoardIcon,
  Queue01Icon,
  ServerStack01Icon,
  Settings02Icon,
  Shield01Icon,
  TaskDaily01Icon,
  Ticket01Icon,
  Wifi01Icon,
  ZapIcon,
} from "@hugeicons/core-free-icons";
import type { IconSvgElement } from "@hugeicons/react";

const ICONS: Record<string, IconSvgElement> = {
  server: ServerStack01Icon,
  database: Database01Icon,
  auth: FingerPrintIcon,
  kv: ZapIcon,
  storage: Folder02Icon,
  functions: CodeIcon,
  jobs: TaskDaily01Icon,
  queues: Queue01Icon,
  realtime: Wifi01Icon,
  vector: AiNetworkIcon,
  graph: HierarchySquare01Icon,
  identity: IdentityCardIcon,
  access: Shield01Icon,
  resp: CommandLineIcon,
  billing: CreditCardIcon,
  entitlements: Ticket01Icon,
  credits: Coins01Icon,
  customize: PaintBoardIcon,
  backups: DatabaseRestoreIcon,
};

export function featureIcon(id: string): IconSvgElement {
  return ICONS[id] ?? Settings02Icon;
}
