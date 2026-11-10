import { Route, Routes } from "react-router-dom";
import { RouteTabs } from "../../components/controls";
import Inspector from "./Inspector";
import SubscriptionDetail from "./SubscriptionDetail";
import SubscriptionList from "./SubscriptionList";
import TopicDetail from "./TopicDetail";
import TopicList from "./TopicList";

/** Queues studio: durable topics, subscriptions, dead letters, replay and a live Pub/Sub inspector (`/api/v1/queues`). */
export default function QueuesPage() {
  return (
    <div>
      <h1 className="text-2xl font-semibold tracking-tight">Queues</h1>
      <RouteTabs label="Queues sections" tabs={[{ to: "/queues", label: "Topics", end: true }, { to: "/queues/subscriptions", label: "Subscriptions" }, { to: "/queues/inspector", label: "Pub/Sub inspector" }]} className="mt-4" />
      <div className="mt-6">
        <Routes>
          <Route index element={<TopicList />} />
          <Route path="subscriptions" element={<SubscriptionList />} />
          <Route path="inspector" element={<Inspector />} />
          <Route path="t/:topic" element={<TopicDetail />} />
          <Route path="s/:sub" element={<SubscriptionDetail />} />
          <Route path="*" element={<p className="text-muted-foreground">No such page in Queues.</p>} />
        </Routes>
      </div>
    </div>
  );
}
