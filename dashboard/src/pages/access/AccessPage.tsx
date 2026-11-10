import { Route, Routes } from "react-router-dom";
import { RouteTabs } from "../../components/controls";
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
      <RouteTabs label="Access sections" tabs={TABS} className="mt-4" />
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
