import { Route, Routes } from "react-router-dom";
import { useState } from "react";
import { listRealmIdentities } from "../../api";
import { PageHeader } from "../../components/PageHeader";
import { HelpTip } from "../../components/HelpTip";
import { PlatformOnly, useAdminScope } from "../../auth/scope";
import { ErrorView, Loading } from "../../components/ui";
import { RouteTabs, Sel } from "../../components/controls";
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
  const { platform } = useAdminScope();
  const realms = useLoad(listRealmIdentities, "idrealms");
  const [picked, setRealm] = useState("default");
  const names = realms.data?.map((r) => r.realm) ?? [];
  const realm = names.length === 0 || names.includes(picked) ? picked : names[0]!;
  return (
    <div>
      <PageHeader
        studio="identity"
        secondary={
          <span className="flex items-end gap-1">
            <Field label="Realm">
              <Sel aria-label="Realm" value={realm} onChange={setRealm} options={names.length ? names : [realm]} />
            </Field>
            <HelpTip term="realm" />
          </span>
        }
      />
      <RouteTabs label="Identity sections" tabs={platform ? TABS : TABS.filter((t) => t.to !== "/identity/clients")} className="mt-4" />
      <div className="mt-6">
        <ErrorView error={realms.error} />
        {realms.loading && !realms.error && <Loading what="Loading realms" />}
        {realms.data && (
          <RealmCtx.Provider value={realm}>
            <Routes>
              <Route index element={<RealmsPage realms={realms.data} reload={realms.reload} onPick={setRealm} />} />
              <Route path="clients" element={<PlatformOnly what="Client apps"><ClientsPage /></PlatformOnly>} />
              <Route path="providers" element={<ProvidersPage realms={realms.data} reload={realms.reload} />} />
              <Route path="consents" element={<ConsentsPage />} />
              <Route path="branding" element={<BrandingPage />} />
              <Route path="domains" element={<DomainsPage />} />
              <Route path="email" element={<EmailPage />} />
              <Route path="legal" element={<AgreementsPage />} />
              <Route path="*" element={<p className="text-muted-foreground">No such page in Identity.</p>} />
            </Routes>
          </RealmCtx.Provider>
        )}
      </div>
    </div>
  );
}
