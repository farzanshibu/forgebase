import { NavLink, Route, Routes } from "react-router-dom";
import { getBillingSettings, isBillingDisabled } from "../../api";
import { Loading } from "../../components/ui";
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

const tab = ({ isActive }: { isActive: boolean }) => `-mb-px whitespace-nowrap border-b-2 px-3 py-2 text-sm ${isActive ? "border-amber-400 text-white" : "border-transparent text-zinc-400 hover:text-zinc-100"}`;

const TABS: { to: string; label: string; end?: boolean }[] = [
  { to: "/billing", label: "Revenue", end: true },
  { to: "/billing/catalog", label: "Catalog" },
  { to: "/billing/customers", label: "Customers" },
  { to: "/billing/subscriptions", label: "Subscriptions" },
  { to: "/billing/orders", label: "Orders" },
  { to: "/billing/usage", label: "Usage meters" },
  { to: "/billing/invoices", label: "Invoices" },
  { to: "/billing/payments", label: "Payments" },
  { to: "/billing/stores", label: "Stores" },
  { to: "/billing/coupons", label: "Coupons" },
  { to: "/billing/dunning", label: "Dunning" },
  { to: "/billing/credits", label: "Credits" },
  { to: "/billing/settings", label: "Providers and settings" },
];

export default function BillingPage() {
  // Billing can be turned off and its routes then answer 404: one probe decides between the studio and the disabled state.
  const probe = useLoad(getBillingSettings, "billing-probe");
  const disabled = isBillingDisabled(probe.error);
  return (
    <div>
      <h1 className="text-2xl font-semibold tracking-tight">Billing Studio</h1>
      {probe.loading && <div className="mt-4"><Loading what="Checking billing" /></div>}
      {!probe.loading && disabled && <div className="mt-6"><Disabled /></div>}
      {!probe.loading && !disabled && (
        <>
          <nav aria-label="Billing sections" className="mt-4 flex gap-1 overflow-x-auto border-b border-zinc-800">
            {TABS.map((t) => <NavLink key={t.to} to={t.to} end={t.end} className={tab}>{t.label}</NavLink>)}
          </nav>
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
              <Route path="*" element={<p className="text-zinc-400">No such page in Billing.</p>} />
            </Routes>
          </div>
        </>
      )}
    </div>
  );
}
