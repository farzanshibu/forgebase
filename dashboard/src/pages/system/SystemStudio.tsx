import { NavLink, Route, Routes } from "react-router-dom";
import SystemPage from "../SystemPage";
import BackupsPage from "./BackupsPage";

const tab = ({ isActive }: { isActive: boolean }) => `-mb-px border-b-2 px-3 py-2 text-sm ${isActive ? "border-primary text-foreground" : "border-transparent text-muted-foreground hover:text-foreground"}`;

/** System studio: live health and build information, and backups (`/api/v1/system/backups`). */
export default function SystemStudio() {
  return (
    <div>
      <nav aria-label="System sections" className="mb-6 flex gap-1 border-b border-border">
        <NavLink to="/system" end className={tab}>Health</NavLink>
        <NavLink to="/system/backups" className={tab}>Backups</NavLink>
      </nav>
      <Routes>
        <Route index element={<SystemPage />} />
        <Route path="backups" element={<BackupsPage />} />
        <Route path="*" element={<p className="text-muted-foreground">No such page in System.</p>} />
      </Routes>
    </div>
  );
}
