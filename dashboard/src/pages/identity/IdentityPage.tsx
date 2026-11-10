import { NavLink, Route, Routes } from "react-router-dom";
import { useState } from "react";
import { listRealmIdentities } from "../../api";
import { ErrorView, Loading, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { Field } from "../access/shared";
import AgreementsPage from "./AgreementsPage";
import BrandingPage from "./BrandingPage";
import ClientsPage from "./ClientsPage";
import ConsentsPage from "./ConsentsPage";
import DomainsPage from "./DomainsPage";
import EmailPage from "./EmailPage";
import ProvidersPage from "./ProvidersPage";
import RealmsPage from "./RealmsPage";
import { RealmCtx } from "./util";

const TABS = [
  { to: "/identity", label: "Realms", end: true },
  { to: "/identity/clients", label: "Client apps", end: false },
  { to: "/identity/providers", label: "Identity providers", end: false },
  { to: "/identity/consents", label: "Consents and keys", end: false },
  { to: "/identity/branding", label: "Branding", end: false },
  { to: "/identity/domains", label: "Custom domains", end: false },
  { to: "/identity/email", label: "Email templates", end: false },
  { to: "/identity/legal", label: "Legal agreements", end: false },
];

/** Identity and Tenant Studio (FB-27): realms, client apps, providers, branding, domains, email and legal over the FB-25/26/74 REST. */
export default function IdentityPage() {
  const realms = useLoad(listRealmIdentities, "idrealms");
  const [picked, setRealm] = useState("default");
  const names = realms.data?.map((r) => r.realm) ?? [];
  const realm = names.length === 0 || names.includes(picked) ? picked : names[0]!;
  return (
    <div>
      <div className="flex flex-wrap items-end justify-between gap-3">
        <h1 className="text-2xl font-semibold tracking-tight">Identity and Tenant Studio</h1>
        <Field label="Realm">
          <select aria-label="Realm" className={inputCls} value={realm} onChange={(e) => setRealm(e.target.value)}>
            {(names.length ? names : [realm]).map((n) => <option key={n} value={n}>{n}</option>)}
          </select>
        </Field>
      </div>
      <nav aria-label="Identity sections" className="mt-4 flex flex-wrap gap-1 border-b border-border">
        {TABS.map((t) => (
          <NavLink key={t.label} to={t.to} end={t.end}
            className={({ isActive }) => `-mb-px border-b-2 px-3 py-2 text-sm ${isActive ? "border-primary text-foreground" : "border-transparent text-muted-foreground hover:text-foreground"}`}>
            {t.label}
          </NavLink>
        ))}
      </nav>
      <div className="mt-6">
        <ErrorView error={realms.error} />
        {realms.loading && !realms.error && <Loading what="Loading realms" />}
        {realms.data && (
          <RealmCtx.Provider value={realm}>
            <Routes>
              <Route index element={<RealmsPage realms={realms.data} reload={realms.reload} onPick={setRealm} />} />
              <Route path="clients" element={<ClientsPage />} />
              <Route path="providers" element={<ProvidersPage realms={realms.data} reload={realms.reload} />} />
              <Route path="consents" element={<ConsentsPage />} />
              <Route path="branding" element={<BrandingPage />} />
              <Route path="domains" element={<DomainsPage />} />
              <Route path="email" element={<EmailPage />} />
              <Route path="legal" element={<AgreementsPage />} />
              <Route path="*" element={<p className="text-muted-foreground">No such page in the Identity Studio.</p>} />
            </Routes>
          </RealmCtx.Provider>
        )}
      </div>
    </div>
  );
}
