import { NavLink, Route, Routes } from "react-router-dom";
import Admins from "./Admins";
import ApiKeys from "./ApiKeys";
import AuditLog from "./AuditLog";
import ServiceAccounts from "./ServiceAccounts";
import UserDetail from "./UserDetail";
import Users from "./Users";

const TABS = [
  { to: "/auth", label: "Users", end: true },
  { to: "/auth/service-accounts", label: "Service accounts", end: false },
  { to: "/auth/keys", label: "API keys & tokens", end: false },
  { to: "/auth/audit", label: "Audit log", end: false },
  { to: "/auth/admins", label: "Admins", end: false },
];

/** Authentication Studio (FB-23, ADR-0014 section 2). */
export default function AuthPage() {
  return (
    <div>
      <h1 className="text-2xl font-semibold tracking-tight">Authentication Studio</h1>
      <nav aria-label="Authentication sections" className="mt-4 flex flex-wrap gap-1 border-b border-border">
        {TABS.map((t) => (
          <NavLink
            key={t.label}
            to={t.to}
            end={t.end}
            className={({ isActive }) =>
              `-mb-px border-b-2 px-3 py-2 text-sm ${isActive ? "border-primary text-foreground" : "border-transparent text-muted-foreground hover:text-foreground"}`
            }
          >
            {t.label}
          </NavLink>
        ))}
      </nav>
      <div className="mt-6">
        <Routes>
          <Route index element={<Users />} />
          <Route path="users/:collection/:id" element={<UserDetail />} />
          <Route path="service-accounts" element={<ServiceAccounts />} />
          <Route path="keys" element={<ApiKeys />} />
          <Route path="audit" element={<AuditLog />} />
          <Route path="admins" element={<Admins />} />
          <Route path="*" element={<p className="text-muted-foreground">No such page in Authentication Studio.</p>} />
        </Routes>
      </div>
    </div>
  );
}
