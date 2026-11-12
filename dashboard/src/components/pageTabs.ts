import { createContext, type ReactNode } from "react";

/**
 * Section tabs a studio wants under every page header (System: Health, Features and settings, Topology, Backups).
 * The studio provides them once; each sub-page keeps its own PageHeader with its own title and actions, and the
 * tabs appear right under it, the same order as every other studio (header, then tabs, then content).
 */
export const PageTabsContext = createContext<ReactNode>(null);
