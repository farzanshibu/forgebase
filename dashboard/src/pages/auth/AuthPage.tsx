import { Route, Routes } from "react-router-dom";
import { RouteTabs } from "../../components/controls";
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
      <RouteTabs label="Authentication sections" tabs={TABS} className="mt-4" />
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
