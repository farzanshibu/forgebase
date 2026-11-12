/**
 * Billing's 13 pages in four groups (audit A-23). The primary tab row shows the groups, a second row the pages of the
 * active group. Every page keeps its own route (`/billing/<page>`).
 *
 * Usage:
 *   groupOf("/billing/coupons")   // "catalog"
 *   GROUPS[0].pages               // [{ to: "/billing/catalog", label: "Products" }, …]
 */
export interface BillingPage {
  to: string;
  label: string;
  end?: boolean;
}

export interface BillingGroup {
  id: "catalog" | "customers" | "money" | "settings";
  label: string;
  pages: BillingPage[];
}

export const GROUPS: BillingGroup[] = [
  { id: "catalog", label: "Catalog", pages: [{ to: "/billing/catalog", label: "Products" }, { to: "/billing/coupons", label: "Coupons" }, { to: "/billing/stores", label: "Stores" }] },
  {
    id: "customers", label: "Customers",
    pages: [{ to: "/billing/customers", label: "Customers" }, { to: "/billing/subscriptions", label: "Subscriptions" }, { to: "/billing/orders", label: "Orders" }, { to: "/billing/usage", label: "Usage" }],
  },
  {
    id: "money", label: "Money",
    pages: [{ to: "/billing", label: "Revenue", end: true }, { to: "/billing/invoices", label: "Invoices" }, { to: "/billing/payments", label: "Payments" }, { to: "/billing/credits", label: "Credits" }, { to: "/billing/dunning", label: "Dunning" }],
  },
  { id: "settings", label: "Settings", pages: [{ to: "/billing/settings", label: "Providers and settings" }] },
];

/** The group a route belongs to; Revenue (`/billing`) is in Money. */
export function groupOf(pathname: string): BillingGroup {
  const here = pathname.replace(/\/+$/, "") || "/";
  for (const g of GROUPS) {
    for (const p of g.pages) if (p.end ? here === p.to : here === p.to || here.startsWith(`${p.to}/`)) return g;
  }
  return GROUPS[2]!;
}
