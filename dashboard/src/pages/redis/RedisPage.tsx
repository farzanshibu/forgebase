import { useState } from "react";
import { NavLink, Route, Routes, useSearchParams } from "react-router-dom";
import { kvNamespaces } from "../../api";
import { inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import Console from "./Console";
import KeyBrowser from "./KeyBrowser";
import PubSub from "./PubSub";
import StatsPanel from "./StatsPanel";

const tab = ({ isActive }: { isActive: boolean }) => `-mb-px border-b-2 px-3 py-2 text-sm ${isActive ? "border-amber-400 text-white" : "border-transparent text-zinc-400 hover:text-zinc-100"}`;

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
            <input list="kv-namespaces" className={`${inputCls} ml-2 w-40`} value={draft} onChange={(e) => setDraft(e.target.value)} onBlur={() => draft.trim() && draft.trim() !== ns && setNs(draft.trim())} />
          </label>
          <datalist id="kv-namespaces">{(names.data ?? []).map((n) => <option key={n.name} value={n.name}>{`${n.keys} keys`}</option>)}</datalist>
        </form>
      </div>
      <nav aria-label="Redis sections" className="mt-4 flex gap-1 border-b border-zinc-800">
        <NavLink to={`/redis${search}`} end className={tab}>Keys</NavLink>
        <NavLink to={`/redis/console${search}`} className={tab}>Console</NavLink>
        <NavLink to={`/redis/pubsub${search}`} className={tab}>Pub/Sub</NavLink>
        <NavLink to={`/redis/stats${search}`} className={tab}>Memory and stats</NavLink>
      </nav>
      <div className="mt-6">
        <Routes>
          <Route index element={<KeyBrowser ns={ns} />} />
          <Route path="console" element={<Console ns={ns} />} />
          <Route path="pubsub" element={<PubSub ns={ns} />} />
          <Route path="stats" element={<StatsPanel ns={ns} namespaces={names.data ?? []} />} />
          <Route path="*" element={<p className="text-zinc-400">No such page in Redis Studio.</p>} />
        </Routes>
      </div>
    </div>
  );
}
