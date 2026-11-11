import { Route, Routes } from "react-router-dom";
import { PlatformOnly } from "../../auth/scope";
import { RouteTabs } from "../../components/controls";
import SystemPage from "../SystemPage";
import BackupsPage from "./BackupsPage";
import FeaturesPage from "../setup/FeaturesPage";

const TABS = [
  { to: "/system", label: "Health", end: true },
  { to: "/system/features", label: "Features and settings" },
  { to: "/system/backups", label: "Backups" },
];

/** System: live health and build information, features and settings, and backups. Each page draws its own PageHeader. Platform administrators only. */
export default function SystemStudio() {
  return (
    <PlatformOnly what="System">
      <RouteTabs label="System sections" tabs={TABS} className="mb-6" />
      <Routes>
        <Route index element={<SystemPage />} />
        <Route path="features" element={<FeaturesPage />} />
        <Route path="backups" element={<BackupsPage />} />
        <Route path="*" element={<p className="text-muted-foreground">No such page in System.</p>} />
      </Routes>
    </PlatformOnly>
  );
}
