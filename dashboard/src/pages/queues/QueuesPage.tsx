import { Outlet, Route, Routes } from "react-router-dom";
import { PageHeader } from "../../components/PageHeader";
import { RouteTabs } from "../../components/controls";
import { Button } from "../../components/ui";
import { useCreateParam } from "../../hooks";
import Catalog from "./Catalog";
import Inspector from "./Inspector";
import NewTopic from "./NewTopic";
import SubscriptionDetail from "./SubscriptionDetail";
import SubscriptionList from "./SubscriptionList";
import TopicDetail from "./TopicDetail";
import TopicList from "./TopicList";

/** The header, the tabs and the New topic sheet belong to the three list pages; a topic or a subscription draws its own header. */
function Home() {
  const [creating, setCreating] = useCreateParam(); // the palette's "New topic" opens it too
  return (
    <div>
      <PageHeader studio="queues" primary={<Button tone="primary" onClick={() => setCreating(true)}>New topic</Button>} />
      <RouteTabs label="Queues sections" tabs={[{ to: "/queues", label: "Topics", end: true }, { to: "/queues/subscriptions", label: "Subscriptions" }, { to: "/queues/catalog", label: "Event catalog" }, { to: "/queues/inspector", label: "Pub/Sub inspector" }]} className="mt-4" />
      <div className="mt-6"><Outlet context={{ newTopic: () => setCreating(true) }} /></div>
      <NewTopic open={creating} onOpenChange={setCreating} />
    </div>
  );
}

/** Queues studio: durable topics, subscriptions, dead letters, replay and a live Pub/Sub inspector (`/api/v1/queues`). */
export default function QueuesPage() {
  return (
    <Routes>
      <Route element={<Home />}>
        <Route index element={<TopicList />} />
        <Route path="subscriptions" element={<SubscriptionList />} />
        <Route path="catalog" element={<Catalog />} />
        <Route path="inspector" element={<Inspector />} />
      </Route>
      <Route path="t/:topic" element={<TopicDetail />} />
      <Route path="s/:sub" element={<SubscriptionDetail />} />
      <Route path="*" element={<p className="text-muted-foreground">No such page in Queues.</p>} />
    </Routes>
  );
}
