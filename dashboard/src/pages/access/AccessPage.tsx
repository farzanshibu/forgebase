import { NavLink, Route, Routes } from "react-router-dom";
import PoliciesPage from "./PoliciesPage";
import RealmPage from "./RealmPage";
import RuleTester from "./RuleTester";
import { OrgDetail, OrgList } from "./OrgsPage";

const TABS = [
  { to: "/access", label: "Organizations", end: true },
  { to: "/access/realm", label: "Realm", end: false },
  { to: "/access/rules", label: "Collection rules", end: false },
  { to: "/access/tester", label: "Rule tester", end: false },
];

/** Access Studio (FB-23): tenancy, roles, permissions, ACL and collection rules over the FB-21 REST. */
export default function AccessPage() {
  return (
    <div>
      <h1 className="text-2xl font-semibold tracking-tight">Access Studio</h1>
      <nav aria-label="Access sections" className="mt-4 flex gap-1 border-b border-border">
        {TABS.map((t) => (
          <NavLink key={t.label} to={t.to} end={t.end}
            className={({ isActive }) => `-mb-px border-b-2 px-3 py-2 text-sm ${isActive ? "border-primary text-foreground" : "border-transparent text-muted-foreground hover:text-foreground"}`}>
            {t.label}
          </NavLink>
        ))}
      </nav>
      <div className="mt-6">
        <Routes>
          <Route index element={<OrgList />} />
          <Route path="orgs/:org/*" element={<OrgDetail />} />
          <Route path="realm" element={<RealmPage />} />
          <Route path="rules" element={<PoliciesPage />} />
          <Route path="tester" element={<RuleTester />} />
          <Route path="*" element={<p className="text-muted-foreground">No such page in Access Studio.</p>} />
        </Routes>
      </div>
    </div>
  );
}
