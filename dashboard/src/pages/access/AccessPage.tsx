import { Route, Routes } from "react-router-dom";
import { EmptyState } from "../../components/EmptyState";
import { PageHeader } from "../../components/PageHeader";
import { RouteTabs } from "../../components/controls";
import PoliciesPage from "./PoliciesPage";
import RealmPage from "./RealmPage";
import RuleTester from "./RuleTester";
import { OrgDetail, OrgList } from "./OrgsPage";

const TABS = [
  // Not `end`: organization pages (/access/orgs/…) keep this tab marked.
  { to: "/access", label: "Organizations", end: false },
  { to: "/access/realm", label: "Realm", end: false },
  { to: "/access/rules", label: "Collection rules", end: false },
  { to: "/access/tester", label: "Rule tester", end: false },
];

/** Access Studio (FB-23): tenancy, roles, permissions, ACL and collection rules over the FB-21 REST. */
export default function AccessPage() {
  return (
    <div>
      <PageHeader studio="access" />
      <RouteTabs label="Access sections" tabs={TABS} className="mt-4" />
      <div className="mt-6">
        <Routes>
          <Route index element={<OrgList />} />
          <Route path="orgs/:org/*" element={<OrgDetail />} />
          <Route path="realm" element={<RealmPage />} />
          <Route path="rules" element={<PoliciesPage />} />
          <Route path="tester" element={<RuleTester />} />
          <Route path="*" element={<EmptyState compact title="No such page in Access" why="The link may be old. Pick a section from the tabs above." />} />
        </Routes>
      </div>
    </div>
  );
}
