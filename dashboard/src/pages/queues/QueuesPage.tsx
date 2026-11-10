import { NavLink, Route, Routes } from "react-router-dom";
import Inspector from "./Inspector";
import SubscriptionDetail from "./SubscriptionDetail";
import SubscriptionList from "./SubscriptionList";
import TopicDetail from "./TopicDetail";
import TopicList from "./TopicList";

const tab = ({ isActive }: { isActive: boolean }) => `-mb-px border-b-2 px-3 py-2 text-sm ${isActive ? "border-primary text-foreground" : "border-transparent text-muted-foreground hover:text-foreground"}`;

/** Queues studio: durable topics, subscriptions, dead letters, replay and a live Pub/Sub inspector (`/api/v1/queues`). */
export default function QueuesPage() {
  return (
    <div>
      <h1 className="text-2xl font-semibold tracking-tight">Queues</h1>
      <nav aria-label="Queues sections" className="mt-4 flex flex-wrap gap-1 border-b border-border">
        <NavLink to="/queues" end className={tab}>Topics</NavLink>
        <NavLink to="/queues/subscriptions" className={tab}>Subscriptions</NavLink>
        <NavLink to="/queues/inspector" className={tab}>Pub/Sub inspector</NavLink>
      </nav>
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
