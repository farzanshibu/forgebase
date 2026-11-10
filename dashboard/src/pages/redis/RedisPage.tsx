import { useState } from "react";
import { Route, Routes, useSearchParams } from "react-router-dom";
import { RouteTabs } from "../../components/controls";
import { kvNamespaces } from "../../api";
import { useLoad } from "../../hooks";
import Console from "./Console";
import KeyBrowser from "./KeyBrowser";
import PubSub from "./PubSub";
import StatsPanel from "./StatsPanel";
import { Input } from "panelui-web/input";

export default function RedisPage() {
  const [params, setParams] = useSearchParams();
  const ns = params.get("ns") || "default";
  const names = useLoad((s) => kvNamespaces(s).catch(() => []), "kv-namespaces");
  const [draft, setDraft] = useState(ns);
  const setNs = (v: string) => {
    const p = new URLSearchParams(params);
    p.delete("key");
    if (v && v !== "default") p.set("ns", v); else p.delete("ns");
    setParams(p);
  };
  const search = ns === "default" ? "" : `?ns=${encodeURIComponent(ns)}`;
  return (
    <div>
      <div className="flex flex-wrap items-end justify-between gap-3">
        <h1 className="text-2xl font-semibold tracking-tight">Redis Studio</h1>
        <form className="flex items-center gap-2 text-sm" onSubmit={(e) => { e.preventDefault(); setNs(draft.trim()); }}>
          <label>Namespace
            <Input size="sm" list="kv-namespaces" className="ml-2 w-40" value={draft} onChange={(e) => setDraft(e.target.value)} onBlur={() => draft.trim() && draft.trim() !== ns && setNs(draft.trim())} />
          </label>
          <datalist id="kv-namespaces">{(names.data ?? []).map((n) => <option key={n.name} value={n.name}>{`${n.keys} keys`}</option>)}</datalist>
        </form>
      </div>
      <RouteTabs label="Redis sections" tabs={[{ to: `/redis${search}`, label: "Keys", end: true }, { to: `/redis/console${search}`, label: "Console" }, { to: `/redis/pubsub${search}`, label: "Pub/Sub" }, { to: `/redis/stats${search}`, label: "Memory and stats" }]} className="mt-4" />
      <div className="mt-6">
        <Routes>
          <Route index element={<KeyBrowser ns={ns} />} />
          <Route path="console" element={<Console ns={ns} />} />
          <Route path="pubsub" element={<PubSub key={ns} ns={ns} />} />
          <Route path="stats" element={<StatsPanel ns={ns} namespaces={names.data ?? []} />} />
          <Route path="*" element={<p className="text-muted-foreground">No such page in Redis Studio.</p>} />
        </Routes>
      </div>
    </div>
  );
}
