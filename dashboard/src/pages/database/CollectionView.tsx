import { Navigate, Route, Routes, useNavigate, useParams } from "react-router-dom";
import { PlusSignIcon } from "@hugeicons/core-free-icons";
import { RouteTabs } from "../../components/controls";
import { listCollections } from "../../api";
import { useLoad } from "../../hooks";
import { EmptyState } from "../../components/EmptyState";
import { PageHeader } from "../../components/PageHeader";
import { SkeletonTable } from "../../components/Skeleton";
import { UseFromCodeButton } from "../../components/UseFromCode";
import { Button, ErrorView } from "../../components/ui";
import CollectionGrid from "./CollectionGrid";
import ImportExport from "./ImportExport";
import IndexManager from "./IndexManager";
import { Facts } from "../../components/Facts";
import { Icon } from "./parts";
import SchemaDesigner from "./SchemaDesigner";

const TABS = [
  { to: "", label: "Data", end: true },
  { to: "/schema", label: "Schema", end: false },
  { to: "/indexes", label: "Indexes", end: false },
  { to: "/transfer", label: "Import / export", end: false },
];

const TYPE_WORDS: Record<string, string> = { base: "Base collection", auth: "Auth collection: users who can sign in", view: "View: a read-only query" };

export default function CollectionView() {
  const { name = "" } = useParams();
  const nav = useNavigate();
  const { data, error, loading, reload } = useLoad(listCollections, "collections");
  const coll = data?.find((c) => c.name === name);
  const base = `/database/c/${encodeURIComponent(name)}`;
  const userFields = coll?.fields.filter((f) => !f.system) ?? [];
  return (
    <div>
      <PageHeader
        title={name}
        description={coll ? (TYPE_WORDS[coll.type] ?? `${coll.type} collection`) : "Collection"}
        docs="docs/database.md"
        secondary={<UseFromCodeButton kind="collection" name={name} />}
        primary={coll?.type !== "view" && (
          <Button tone="primary" onClick={() => nav(`${base}?record=new`)}>
            <Icon icon={PlusSignIcon} size={14} />
            Add record
          </Button>
        )}
      >
        {coll && (
          <Facts
            className="mt-3"
            label="Collection facts"
            items={[
              ["Type", coll.type],
              ["Fields", <span className="tabular-nums">{userFields.length}</span>],
              ["Schema", coll.schema_mode ?? "strict"],
              ["Record versions", coll.versioned ? "Kept" : "Off"],
              ...(coll.system ? [["Managed by", "ForgeBase"] as [string, string]] : []),
            ]}
          />
        )}
      </PageHeader>
      <RouteTabs label="Collection sections" tabs={TABS.map((t) => ({ ...t, to: `${base}${t.to}` }))} className="mt-5 mb-5" />
      <ErrorView error={error} onRetry={reload} />
      {loading && !error && <SkeletonTable rows={4} columns={4} label="Loading collection" />}
      {data && !coll && (
        <EmptyState
          title={`No collection named ${name}`}
          why="It may have been deleted or renamed. Pick one from the list of collections."
          action={<Button onClick={() => nav("/database")}>All collections</Button>}
        />
      )}
      {coll && (
        <Routes>
          <Route index element={<CollectionGrid collection={coll} collections={data ?? []} />} />
          <Route path="r/:id" element={<LegacyRecord base={base} />} />
          <Route path="schema" element={<SchemaDesigner existing={coll} onSaved={reload} />} />
          <Route path="indexes" element={<IndexManager collection={coll.name} />} />
          <Route path="transfer" element={<ImportExport collection={coll} />} />
        </Routes>
      )}
    </div>
  );
}

/** Old deep links `/c/x/r/<id>` open the record sheet on the grid. */
function LegacyRecord({ base }: { base: string }) {
  const { id = "" } = useParams();
  return <Navigate replace to={`${base}?record=${encodeURIComponent(id)}`} />;
}
