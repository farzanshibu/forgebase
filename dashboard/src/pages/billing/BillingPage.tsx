import { Link, Route, Routes, useLocation } from "react-router-dom";
import { getBillingSettings, isBillingDisabled } from "../../api";
import { activeTab } from "../../components/controls";
import { EmptyState } from "../../components/EmptyState";
import { PageHeader } from "../../components/PageHeader";
import { SkeletonCards } from "../../components/Skeleton";
import { PlatformOnly } from "../../auth/scope";
import { Tabs } from "panelui-web/tabs";
import { GROUPS, groupOf, type BillingPage as Page } from "./groups";
import { useLoad } from "../../hooks";
import Catalog from "./Catalog";
import Coupons from "./Coupons";
import Credits from "./Credits";
import Customers from "./Customers";
import Dunning from "./Dunning";
import Invoices from "./Invoices";
import Orders from "./Orders";
import Payments from "./Payments";
import Revenue from "./Revenue";
import Settings from "./Settings";
import { Disabled } from "./shared";
import Stores from "./Stores";
import Subscriptions from "./Subscriptions";
import Usage from "./Usage";

/** The pages of the active group, as a compact segmented control under the group tabs. */
function PageTabs({ label, pages }: { label: string; pages: readonly Page[] }) {
  const { pathname } = useLocation();
  return (
    <Tabs variant="segmented" value={activeTab(pages, pathname) ?? ""} className="w-fit max-w-full">
      <Tabs.List aria-label={label}>
        {pages.map((p) => (
          <Tabs.Trigger key={p.to} value={p.to} nativeButton={false} render={<Link to={p.to} />}>
            {p.label}
          </Tabs.Trigger>
        ))}
      </Tabs.List>
    </Tabs>
  );
}

export default function BillingPage() {
  // Billing can be turned off and its routes then answer 404: one probe decides between the studio and the disabled state.
  const probe = useLoad(getBillingSettings, "billing-probe");
  const disabled = isBillingDisabled(probe.error);
  const group = groupOf(useLocation().pathname);
  return (
    <div>
      <PageHeader studio="billing" />
      <PlatformOnly what="Billing">
        {probe.loading && (
          <div className="mt-6">
            <SkeletonCards count={3} label="Checking billing" />
          </div>
        )}
        {!probe.loading && disabled && <div className="mt-6"><Disabled /></div>}
        {!probe.loading && !disabled && (
          <>
            <Tabs variant="underline" value={group.id} className="mt-4">
              <Tabs.List aria-label="Billing groups">
                {GROUPS.map((g) => (
                  <Tabs.Trigger key={g.id} value={g.id} nativeButton={false} render={<Link to={g.pages[0]!.to} />}>
                    {g.label}
                  </Tabs.Trigger>
                ))}
              </Tabs.List>
            </Tabs>
            {group.pages.length > 1 && (
              <div className="mt-4">
                <PageTabs label={`${group.label} pages`} pages={group.pages} />
              </div>
            )}
            <div className="mt-5">
              <Routes>
                <Route index element={<Revenue />} />
                <Route path="catalog" element={<Catalog />} />
                <Route path="customers" element={<Customers />} />
                <Route path="subscriptions" element={<Subscriptions />} />
                <Route path="orders" element={<Orders />} />
                <Route path="usage" element={<Usage />} />
                <Route path="invoices/*" element={<Invoices />} />
                <Route path="payments" element={<Payments />} />
                <Route path="stores" element={<Stores />} />
                <Route path="coupons" element={<Coupons />} />
                <Route path="dunning" element={<Dunning />} />
                <Route path="credits" element={<Credits />} />
                <Route path="settings" element={<Settings />} />
                <Route path="*" element={<EmptyState title="No such page in Billing" why="Pick a section from the tabs above." />} />
              </Routes>
            </div>
          </>
        )}
      </PlatformOnly>
    </div>
  );
}
