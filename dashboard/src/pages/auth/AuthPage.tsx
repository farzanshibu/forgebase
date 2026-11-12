import { Route, Routes, useNavigate } from "react-router-dom";
import { useAdminScope, PlatformOnly } from "../../auth/scope";
import { RouteTabs } from "../../components/controls";
import { EmptyState } from "../../components/EmptyState";
import { PageHeader } from "../../components/PageHeader";
import { Button } from "../../components/ui";
import Admins from "./Admins";
import ApiKeys from "./ApiKeys";
import AuditLog from "./AuditLog";
import ServiceAccounts from "./ServiceAccounts";
import UserDetail from "./UserDetail";
import Users from "./Users";

const TABS = [
  // Not `end`: user detail pages (/auth/users/…) keep the Users tab marked.
  { to: "/auth", label: "Users", end: false },
  { to: "/auth/keys", label: "API keys", end: false },
  { to: "/auth/service-accounts", label: "Service accounts", end: false },
  { to: "/auth/audit", label: "Audit log", end: false },
  { to: "/auth/admins", label: "Admins", end: false },
];

/** Authentication: users, API keys, service accounts, the audit log and administrators (ADR-0014 section 2). */
export default function AuthPage() {
  const navigate = useNavigate();
  const { platform } = useAdminScope();
  return (
    <div>
      <PageHeader studio="auth" primary={<Button tone="primary" onClick={() => navigate("/auth/keys?new=1")}>Create API key</Button>} />
      {/* Platform administrators are managed by the platform: a realm administrator does not see the tab. */}
      <RouteTabs label="Authentication sections" tabs={platform ? TABS : TABS.filter((t) => t.to !== "/auth/admins")} className="mt-4" />
      <div className="mt-6">
        <Routes>
          <Route index element={<Users />} />
          <Route path="users/:collection/:id" element={<UserDetail />} />
          <Route path="service-accounts" element={<ServiceAccounts />} />
          <Route path="keys" element={<ApiKeys />} />
          <Route path="audit" element={<AuditLog />} />
          <Route path="admins" element={<PlatformOnly what="Administrators"><Admins /></PlatformOnly>} />
          <Route path="*" element={<EmptyState compact title="No such page in Authentication" why="The link may be old. Pick a section from the tabs above." />} />
        </Routes>
      </div>
    </div>
  );
}
