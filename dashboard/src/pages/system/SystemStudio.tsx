import { Route, Routes } from "react-router-dom";
import { RouteTabs } from "../../components/controls";
import SystemPage from "../SystemPage";
import BackupsPage from "./BackupsPage";
import FeaturesPage from "../setup/FeaturesPage";

const TABS = [
  { to: "/system", label: "Health", end: true },
  { to: "/system/features", label: "Features & settings" },
  { to: "/system/backups", label: "Backups" },
];

/** System studio: live health and build information, and backups (`/api/v1/system/backups`). */
export default function SystemStudio() {
  return (
    <div>
      <RouteTabs label="System sections" tabs={TABS} className="mb-6" />
      <Routes>
        <Route index element={<SystemPage />} />
        <Route path="features" element={<FeaturesPage />} />
        <Route path="backups" element={<BackupsPage />} />
        <Route path="*" element={<p className="text-muted-foreground">No such page in System.</p>} />
      </Routes>
    </div>
  );
}
