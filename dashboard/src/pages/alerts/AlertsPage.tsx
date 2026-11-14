import { Outlet, Route, Routes, useLocation } from "react-router-dom";
import { PlatformOnly } from "../../auth/scope";
import { PageHeader } from "../../components/PageHeader";
import { RouteTabs } from "../../components/controls";
import { Button } from "../../components/ui";
import { useCreateParam } from "../../hooks";
import ActiveAlerts from "./ActiveAlerts";
import History from "./History";
import Notifiers from "./Notifiers";
import Rules from "./Rules";
import Silences from "./Silences";
import Slos from "./Slos";

const TABS = [
  { to: "/alerts", label: "Alerts", end: true },
  { to: "/alerts/history", label: "History" },
  { to: "/alerts/rules", label: "Rules" },
  { to: "/alerts/silences", label: "Silences" },
  { to: "/alerts/notifiers", label: "Notifiers" },
  { to: "/alerts/slos", label: "SLOs" },
];

/** The primary action of each tab that creates something (its page opens the sheet on `?new=1`); the others have none. */
const NEW: Record<string, string> = { rules: "New rule", silences: "New silence", notifiers: "New notifier", slos: "New SLO" };

function Home() {
  const [, setCreating] = useCreateParam();
  const tab = useLocation().pathname.split("/").filter(Boolean)[1] ?? "";
  const label = NEW[tab];
  return (
    <div>
      <PageHeader studio="alerts" primary={label ? <Button tone="primary" onClick={() => setCreating(true)}>{label}</Button> : undefined} />
      <RouteTabs label="Alerts sections" tabs={TABS} className="mt-4" />
      <div className="mt-6"><Outlet /></div>
    </div>
  );
}

/** Alerts and SLOs (ADR-0035): open alerts and their history, rules, silences, notifiers and service level objectives. */
export default function AlertsPage() {
  return (
    <PlatformOnly what="Alerts">
      <Routes>
        <Route element={<Home />}>
          <Route index element={<ActiveAlerts />} />
          <Route path="history" element={<History />} />
          <Route path="rules" element={<Rules />} />
          <Route path="silences" element={<Silences />} />
          <Route path="notifiers" element={<Notifiers />} />
          <Route path="slos" element={<Slos />} />
        </Route>
        <Route path="*" element={<p className="text-muted-foreground">No such page in Alerts.</p>} />
      </Routes>
    </PlatformOnly>
  );
}
