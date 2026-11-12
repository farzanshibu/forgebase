import { useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { Database01Icon, Search01Icon, UserGroupIcon, ViewIcon } from "@hugeicons/core-free-icons";
import { Badge, type BadgeVariant } from "panelui-web/badge";
import { Input } from "panelui-web/input";
import { Table } from "panelui-web/table";
import { listCollections } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { SkeletonTable } from "../../components/Skeleton";
import { Button, ErrorView } from "../../components/ui";
import { useLoad } from "../../hooks";
import { Icon, TableCard } from "./parts";

const KIND: Record<string, { icon: typeof Database01Icon; tone: BadgeVariant }> = {
  base: { icon: Database01Icon, tone: "secondary" },
  auth: { icon: UserGroupIcon, tone: "info" },
  view: { icon: ViewIcon, tone: "outline" },
};

export default function CollectionBrowser() {
  const nav = useNavigate();
  const { data, error, loading, reload } = useLoad(listCollections, "collections");
  const [q, setQ] = useState("");
  const shown = (data ?? []).filter((c) => c.name.toLowerCase().includes(q.trim().toLowerCase()));
  const open = (name: string) => nav(`/database/c/${encodeURIComponent(name)}`);
  return (
    <div>
      <ErrorView error={error} onRetry={reload} />
      {loading && !error && <SkeletonTable rows={4} columns={5} label="Loading collections" />}
      {data && data.length === 0 && (
        <EmptyState
          title="No collections yet"
          why="A collection stores records that share a schema, like a table of products or orders. Create one, then add records and read them from your app."
          action={<Button tone="primary" onClick={() => nav("/database/new")}>New collection</Button>}
          icon={Database01Icon}
          docs="docs/database.md"
        />
      )}
      {data && data.length > 0 && (
        <TableCard
          title="Collections"
          count={data.length}
          actions={
            <label className="relative block">
              <span className="pointer-events-none absolute inset-y-0 left-2.5 flex items-center text-muted-foreground"><Icon icon={Search01Icon} size={14} /></span>
              <Input size="sm" type="search" aria-label="Find a collection" placeholder="Find a collection" className="w-56 pl-8" value={q} onChange={(e) => setQ(e.target.value)} />
            </label>
          }
        >
          {shown.length === 0 ? (
            <div className="p-4"><EmptyState compact title="No collections match" why="Clear the search to see every collection." /></div>
          ) : (
            <Table framed density="compact" aria-label="Collections">
              <Table.Header>
                <Table.Row>
                  <Table.Head>Name</Table.Head>
                  <Table.Head>Type</Table.Head>
                  <Table.Head>Schema</Table.Head>
                  <Table.Head align="end">Fields</Table.Head>
                  <Table.Head align="end">Version</Table.Head>
                </Table.Row>
              </Table.Header>
              <Table.Body>
                {shown.map((c) => {
                  const kind = KIND[c.type] ?? KIND.base!;
                  return (
                    <Table.Row
                      key={c.name}
                      className="cursor-pointer"
                      onClick={(e) => {
                        if ((e.target as HTMLElement).closest("a, button")) return;
                        open(c.name);
                      }}
                    >
                      <Table.Cell className="py-2.5">
                        <span className="flex items-center gap-2.5">
                          <span className="flex size-7 items-center justify-center rounded-md bg-muted text-muted-foreground"><Icon icon={kind.icon} size={15} /></span>
                          <Link className="font-medium text-foreground hover:text-primary hover:underline" to={`/database/c/${encodeURIComponent(c.name)}`}>
                            {c.name}
                          </Link>
                          {c.system && <Badge variant="outline" size="sm" title="Created and managed by ForgeBase">system</Badge>}
                        </span>
                      </Table.Cell>
                      <Table.Cell><Badge variant={kind.tone} size="sm">{c.type}</Badge></Table.Cell>
                      <Table.Cell className="text-muted-foreground">{c.schema_mode ?? "strict"}</Table.Cell>
                      <Table.Cell align="end">{c.fields.filter((f) => !f.system).length}</Table.Cell>
                      <Table.Cell align="end" className="text-muted-foreground">{c.version !== undefined ? `v${c.version}` : "–"}</Table.Cell>
                    </Table.Row>
                  );
                })}
              </Table.Body>
            </Table>
          )}
        </TableCard>
      )}
    </div>
  );
}
