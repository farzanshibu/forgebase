import { Route, Routes } from "react-router-dom";
import { PlatformOnly } from "../../auth/scope";
import { RouteTabs } from "../../components/controls";
import { PageTabsContext } from "../../components/pageTabs";
import SystemPage from "../SystemPage";
import BackupsPage from "./BackupsPage";
import FeaturesPage from "../setup/FeaturesPage";
import TopologyPage from "./topology/TopologyPage";

const TABS = [
  { to: "/system", label: "Health", end: true },
  { to: "/system/features", label: "Features and settings" },
  { to: "/system/topology", label: "Database topology" },
  { to: "/system/backups", label: "Backups" },
];

/**
 * System: live health and build information, features and settings, database topology (replicas and shards), and backups.
 * Each page draws its own PageHeader; the section tabs show under it (PageTabsContext), as in the other studios.
 * Platform administrators only.
 */
export default function SystemStudio() {
  return (
    <PlatformOnly what="System">
      <PageTabsContext.Provider value={<RouteTabs label="System sections" tabs={TABS} />}>
        <Routes>
          <Route index element={<SystemPage />} />
          <Route path="features" element={<FeaturesPage />} />
          <Route path="topology" element={<TopologyPage />} />
          <Route path="backups" element={<BackupsPage />} />
          <Route path="*" element={<p className="text-muted-foreground">No such page in System.</p>} />
        </Routes>
      </PageTabsContext.Provider>
    </PlatformOnly>
  );
}
