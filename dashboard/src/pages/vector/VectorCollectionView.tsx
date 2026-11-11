import { Route, Routes, useParams } from "react-router-dom";
import { RouteTabs } from "../../components/controls";
import { getVectorCollection } from "../../api";
import { PageHeader } from "../../components/PageHeader";
import { SkeletonLines } from "../../components/Skeleton";
import { UseFromCodeButton } from "../../components/UseFromCode";
import { ErrorView } from "../../components/ui";
import { serverUrl, vectorCollection } from "../../snippets";
import { useLoad } from "../../hooks";
import ResourceRules from "../../components/ResourceRules";
import VectorIndex from "./VectorIndex";
import VectorRecords from "./VectorRecords";
import VectorSearch from "./VectorSearch";

export default function VectorCollectionView() {
  const { name = "" } = useParams();
  const { data, error, reload } = useLoad((s) => getVectorCollection(name, s), `vc/${name}`);
  const base = `/vector/c/${encodeURIComponent(name)}`;
  const tabs = [
    { to: base, label: "Records", end: true },
    { to: `${base}/search`, label: "Search", end: false },
    { to: `${base}/index`, label: "Index", end: false },
    { to: `${base}/rules`, label: "Rules", end: false },
  ];
  if (error) return <ErrorView error={error} onRetry={reload} />;
  if (!data) return <SkeletonLines lines={4} label="Loading collection" />;
  const first = data.fields[0];
  const source = (data as { source_collection?: string | null }).source_collection;
  return (
    <div>
      <PageHeader
        title={data.name}
        description={`${data.description || "Vector collection"} · ${data.items} items · fields: ${data.fields.map((f) => `${f.name} (${f.dimension}, ${f.metric}, ${f.quantization})`).join(", ") || "none"}`}
        docs="docs/adr/0012-vector.md"
        secondary={<UseFromCodeButton snippet={vectorCollection(serverUrl(), data.name, first?.name, first?.dimension)} />}
      >
        {source && <p className="mt-1 text-sm text-muted-foreground">Source collection: <span className="font-mono">{source}</span>. Hits are re-checked against its View rule and field mask.</p>}
      </PageHeader>
      <RouteTabs label="Collection sections" tabs={tabs} className="mt-4" />
      <div className="mt-4">
        <Routes>
          <Route index element={<VectorRecords collection={data} onChanged={reload} />} />
          <Route path="search" element={<VectorSearch collection={data} />} />
          <Route path="index" element={<VectorIndex collection={data} />} />
          <Route path="rules" element={<VectorRules name={data.name} />} />
        </Routes>
      </div>
    </div>
  );
}

/** Collection rules (FB-139): a rule over `id` and `metadata.*` is ANDed into the search before the top results are picked. */
function VectorRules({ name }: { name: string }) {
  return (
    <div className="space-y-3">
      <p className="text-sm text-muted-foreground">
        A rule can test <code className="font-mono">id</code> and <code className="font-mono">metadata.&lt;key&gt;</code>, such as <code className="font-mono">metadata.owner = @request.auth.id</code>. It filters searches before the top results are chosen, and checks writes before and after the change. Callers without a tenant need a tenant, unless they are administrators.
      </p>
      <ResourceRules kind="vector_collection" name={name} noun="collection" />
    </div>
  );
}
