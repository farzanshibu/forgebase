import { Link, Route, Routes, useLocation } from "react-router-dom";
import { getBillingSettings, isBillingDisabled } from "../../api";
import { Loading } from "../../components/ui";
import { RouteTabs } from "../../components/controls";
import { PageHeader } from "../../components/PageHeader";
import { PlatformOnly } from "../../auth/scope";
import { Tabs } from "panelui-web/tabs";
import { GROUPS, groupOf } from "./groups";
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

export default function BillingPage() {
  // Billing can be turned off and its routes then answer 404: one probe decides between the studio and the disabled state.
  const probe = useLoad(getBillingSettings, "billing-probe");
  const disabled = isBillingDisabled(probe.error);
  const group = groupOf(useLocation().pathname);
  return (
    <div>
      <PageHeader studio="billing" />
      <PlatformOnly what="Billing">
      {probe.loading && <div className="mt-4"><Loading what="Checking billing" /></div>}
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
          {group.pages.length > 1 && <RouteTabs label={`${group.label} pages`} tabs={group.pages} className="mt-2" />}
          <div className="mt-6">
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
              <Route path="*" element={<p className="text-muted-foreground">No such page in Billing.</p>} />
            </Routes>
          </div>
        </>
      )}
      </PlatformOnly>
    </div>
  );
}
