import { useState } from "react";
import { Link, useNavigate, useParams } from "react-router-dom";
import { createRecord, getRecord, replaceRecord } from "../../api";
import type { CollectionDef } from "../../api";
import { useLoad } from "../../hooks";
import { Button, ErrorView, Loading } from "../../components/ui";
import { READ_ONLY_KEYS } from "./values";

/** JSON document editor: `r/new` creates, `r/:id` replaces with `If-Match` on the loaded version. */
export default function RecordEditor({ collection }: { collection: CollectionDef }) {
  const { id = "" } = useParams();
  const isNew = id === "new";
  const { data, error, loading } = useLoad((signal) => (isNew ? Promise.resolve(undefined) : getRecord(collection.name, id, signal)), `${collection.name}/${id}`);
  if (error) return <ErrorView error={error} />;
  if (loading) return <Loading what="Loading record" />;
  return <Editor key={id} collection={collection} id={id} initial={data ? JSON.stringify(data, null, 2) : "{\n  \n}"} version={data?._version} />;
}

function Editor({ collection, id, initial, version }: { collection: CollectionDef; id: string; initial: string; version?: number }) {
  const nav = useNavigate();
  const [text, setText] = useState(initial);
  const [problem, setProblem] = useState<unknown>();
  const [saved, setSaved] = useState(false);
  const isNew = id === "new";
  const base = `/database/c/${encodeURIComponent(collection.name)}`;

  async function save() {
    setSaved(false);
    let doc: unknown;
    try {
      doc = JSON.parse(text);
    } catch (e) {
      setProblem(new Error(`Invalid JSON: ${(e as Error).message}`));
      return;
    }
    if (typeof doc !== "object" || doc === null || Array.isArray(doc)) {
      setProblem(new Error("A record must be a JSON object"));
      return;
    }
    const body = { ...(doc as Record<string, unknown>) };
    for (const k of READ_ONLY_KEYS) delete body[k];
    try {
      if (isNew) {
        const rec = await createRecord(collection.name, body);
        nav(`${base}/r/${encodeURIComponent(String(rec.id))}`);
      } else {
        await replaceRecord(collection.name, id, body, version);
        setProblem(undefined);
        setSaved(true);
      }
    } catch (e) {
      setProblem(e);
    }
  }

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between text-sm">
        <span className="text-zinc-400">{isNew ? "New record" : `Record ${id}${version ? ` · version ${version}` : ""}`}</span>
        <Link className="text-amber-300 hover:underline" to={base}>Back to data</Link>
      </div>
      <textarea
        aria-label="Record JSON"
        spellCheck={false}
        className="h-96 w-full rounded-md border border-zinc-700 bg-zinc-900 p-3 font-mono text-xs"
        value={text}
        onChange={(e) => setText(e.target.value)}
      />
      <ErrorView error={problem} />
      {saved && <p role="status" className="text-sm text-emerald-300">Saved.</p>}
      <Button tone="primary" onClick={() => void save()}>{isNew ? "Create record" : "Save record"}</Button>
    </div>
  );
}
