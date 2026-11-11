import { useEffect, useState } from "react";
import { Route, Routes, useSearchParams } from "react-router-dom";
import { Input } from "panelui-web/input";
import { kvNamespaces } from "../../api";
import { RouteTabs } from "../../components/controls";
import { FormField } from "../../components/FormField";
import { PageHeader } from "../../components/PageHeader";
import { UseFromCodeButton } from "../../components/UseFromCode";
import { Button } from "../../components/ui";
import { useCreateParam, useLoad } from "../../hooks";
import { validate } from "../../validate";
import Console from "./Console";
import KeyBrowser from "./KeyBrowser";
import PubSub from "./PubSub";
import Rules from "./Rules";
import StatsPanel from "./StatsPanel";

export default function RedisPage() {
  const [params, setParams] = useSearchParams();
  const ns = params.get("ns") || "default";
  const names = useLoad((s) => kvNamespaces(s).catch(() => []), "kv-namespaces");
  const [draft, setDraft] = useState(ns);
  useEffect(() => setDraft(ns), [ns]);
  const [creating, setCreating] = useCreateParam();
  const draftError = draft.trim() && draft.trim() !== ns ? validate.namespaceName(draft.trim()) : undefined;
  /** The namespace lives in the URL (`?ns=`), so a link or a reload keeps it. */
  const setNs = (v: string) => {
    const p = new URLSearchParams(params);
    p.delete("key");
    p.delete("new");
    if (v) p.set("ns", v); else p.delete("ns");
    setParams(p);
  };
  const search = params.get("ns") ? `?ns=${encodeURIComponent(ns)}` : "";
  return (
    <div>
      <PageHeader
        studio="redis"
        secondary={<UseFromCodeButton kind="kvNamespace" name={ns} />}
        primary={<Button tone="primary" onClick={() => setCreating(true)}>New key</Button>}
      />
      <form className="mt-4 flex flex-wrap items-start gap-2 text-sm" onSubmit={(e) => { e.preventDefault(); if (!draftError && draft.trim()) setNs(draft.trim()); }}>
        <FormField label="Namespace" term="namespace" error={draftError} help={<>Names starting with <code className="font-mono">shared.</code> are shared by every tenant of a realm.</>}>
          {(p) => <Input {...p} size="sm" list="kv-namespaces" className="w-48" value={draft} onChange={(e) => setDraft(e.target.value)} onBlur={() => draft.trim() && draft.trim() !== ns && !draftError && setNs(draft.trim())} />}
        </FormField>
        <datalist id="kv-namespaces">{(names.data ?? []).map((n) => <option key={n.name} value={n.name}>{`${n.keys} keys`}</option>)}</datalist>
      </form>
      <RouteTabs label="Key-value sections" tabs={[{ to: `/redis${search}`, label: "Keys", end: true }, { to: `/redis/console${search}`, label: "Console" }, { to: `/redis/pubsub${search}`, label: "Pub/Sub" }, { to: `/redis/stats${search}`, label: "Memory and stats" }, { to: `/redis/rules${search}`, label: "Rules" }]} className="mt-4" />
      <div className="mt-6">
        <Routes>
          <Route index element={<KeyBrowser ns={ns} creating={creating} setCreating={setCreating} />} />
          <Route path="console" element={<Console ns={ns} />} />
          <Route path="pubsub" element={<PubSub key={ns} ns={ns} />} />
          <Route path="stats" element={<StatsPanel ns={ns} namespaces={names.data ?? []} />} />
          <Route path="rules" element={<Rules ns={ns} />} />
          <Route path="*" element={<p className="text-muted-foreground">No such page in the Key-value studio.</p>} />
        </Routes>
      </div>
    </div>
  );
}
