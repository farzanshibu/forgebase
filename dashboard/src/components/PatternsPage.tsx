/**
 * /_patterns (dev builds only): every shared component on one page, to check a change in both themes without
 * finding a screen that uses it. Not linked from the nav; App.tsx drops the route from production builds.
 * docs/ux/components.md explains which component to use for which job.
 */
import { Delete02Icon } from "@hugeicons/core-free-icons";
import { Input } from "panelui-web/input";
import { Table } from "panelui-web/table";
import { useState } from "react";
import { ApiError } from "../api";
import { useCreateParam } from "../hooks";
import { notify } from "../toaster";
import { validate } from "../validate";
import { Button, CopyButton, CopyIcon } from "./Button";
import { Chip, Chips } from "./Chips";
import { ConfirmDialog } from "./ConfirmDialog";
import { Check, Sel, Toggle } from "./controls";
import { CreateSheet } from "./CreateSheet";
import { EmptyState } from "./EmptyState";
import { ErrorPanel } from "./ErrorPanel";
import { Facts } from "./Facts";
import { FormField } from "./FormField";
import { HelpTip } from "./HelpTip";
import { IdText } from "./IdText";
import { PageHeader } from "./PageHeader";
import { Pager } from "./Pager";
import { SkeletonCards, SkeletonTable } from "./Skeleton";
import { Stat, StatGrid } from "./Stats";
import { Badge, Dot } from "./StatusBadge";
import { statusTone } from "./statusTone";
import { CardBody, HEAD_ROW, NUM, TableCard, Th } from "./TableCard";
import { When } from "./Time";
import { ConfirmButton, IconButton, Panel } from "./ui";
import { UseFromCode, UseFromCodeButton } from "./UseFromCode";

const STATUSES = ["succeeded", "queued", "running", "past_due", "cancelled", "draft", "expired", "paid"];
const ago = (min: number) => new Date(Date.now() - min * 60_000).toISOString();

const err = (status: number, code: string, message: string, details?: unknown) =>
  new ApiError({ status, code, message, requestId: "req-7f3a", body: { error: { code, message, request_id: "req-7f3a", ...(details ? { details } : {}) } } });

export default function PatternsPage() {
  const [name, setName] = useState("");
  const [touched, setTouched] = useState(false);
  const [page, setPage] = useState(2);
  const [shown, setShown] = useState(20);
  const [on, setOn] = useState(true);
  const [check, setCheck] = useState(false);
  const [vis, setVis] = useState("private");
  const [creating, setCreating] = useCreateParam();
  const nameError = touched ? validate.collectionName(name) : undefined;
  return (
    <div className="space-y-8">
      <PageHeader
        title="Shared patterns"
        description="Every shared component, for checking a change in light and dark."
        docs="docs/ux/components.md"
        secondary={<UseFromCodeButton kind="collection" name="posts" />}
        primary={
          <Button tone="primary" onClick={() => setCreating(true)}>
            New thing
          </Button>
        }
      />
      <Panel title="EmptyState">
        <EmptyState
          title="No buckets yet"
          why="Buckets hold files your app uploads, like avatars or exports."
          action={<Button tone="primary">New bucket</Button>}
          sample={<Button>Try a sample</Button>}
          docs="docs/adr/0018-object-storage.md"
        />
      </Panel>
      <Panel title="ErrorPanel">
        <div className="space-y-3">
          <ErrorPanel error={err(400, "invalid_name", 'invalid name "": use letters, digits and underscores')} />
          <ErrorPanel error={err(403, "forbidden", "missing permission collections:write")} />
          <ErrorPanel error={err(401, "unauthenticated", "authentication required")} />
          <ErrorPanel error={err(400, "validation_failed", "validation failed", { fields: { title: { code: "required", message: "is required" } } })} onRetry={() => notify.saved("Nothing")} />
        </div>
      </Panel>
      <Panel title="FormField, HelpTip and controls">
        <form className="grid max-w-lg gap-4" onSubmit={(e) => e.preventDefault()}>
          <FormField label="Collection name" required help="Letters, digits and underscores, starting with a letter." error={nameError}>
            {(p) => <Input {...p} value={name} onChange={(e) => setName(e.target.value)} onBlur={() => setTouched(true)} />}
          </FormField>
          <FormField label="Visibility" term="acl">
            {(p) => <Sel {...p} aria-label="Visibility" value={vis} onChange={setVis} options={["private", "public"]} />}
          </FormField>
          <Check checked={check} onChange={setCheck} label="Also delete its files" description="Off keeps the files." />
          <Toggle checked={on} onChange={setOn} label="Versioning" description="Keep earlier versions of each file." />
          <p className="text-sm">
            Keys expire after their <HelpTip term="ttl">TTL</HelpTip>.
          </p>
        </form>
      </Panel>
      <Panel title="UseFromCode">
        <UseFromCode kind="topic" name="orders" />
      </Panel>
      <Panel title="ConfirmDialog, toasts and copy">
        <div className="flex flex-wrap gap-2">
          <ConfirmDialog
            trigger={<Button tone="danger">Delete bucket avatars</Button>}
            title="Delete bucket avatars?"
            consequence="Every file in it is deleted. Apps that read it get 404."
            confirmLabel="Delete bucket"
            confirmText="avatars"
            onConfirm={() => void notify.deleted("Bucket avatars")}
          />
          <Button onClick={() => notify.saved("Schema")}>Toast: saved</Button>
          <Button onClick={() => notify.created("Bucket avatars", "/storage/b/avatars")}>Toast: created</Button>
          <Button onClick={() => notify.failed(err(409, "already_exists", "bucket exists"))}>Toast: failed</Button>
          <CopyButton text="req-7f3a" label="Copy request id" />
        </div>
      </Panel>
      <TableCard
        title="TableCard"
        count={2}
        description="A list or table in a bordered card: title, count, one sentence, actions; a toolbar row; a footer for the Pager."
        actions={<Button>Refresh</Button>}
        toolbar={<Input size="sm" type="search" aria-label="Find an invoice" placeholder="Find an invoice" className="w-56" />}
        footer={<Pager page={1} perPage={25} total={2} onPage={() => undefined} label="Invoice pages" />}
      >
        <Table framed density="compact" aria-label="Invoices">
          <Table.Header className={HEAD_ROW}>
            <Table.Row><Th>Invoice</Th><Th>Status</Th><Th>Created</Th><Th align="end">Amount</Th><Th align="end"><span className="sr-only">Actions</span></Th></Table.Row>
          </Table.Header>
          <Table.Body>
            {[{ id: "in_7f3a9c2e41", status: "paid", at: ago(5), amount: "$12.99" }, { id: "in_a1b2c3d4e5", status: "past_due", at: ago(60 * 30), amount: "$129.00" }].map((r) => (
              <Table.Row key={r.id}>
                <Table.Cell><IdText id={r.id} what="invoice id" /></Table.Cell>
                <Table.Cell><Badge status={r.status} /></Table.Cell>
                <Table.Cell><When value={r.at} /></Table.Cell>
                <Table.Cell className={NUM}>{r.amount}</Table.Cell>
                <Table.Cell align="end"><IconButton icon={Delete02Icon} label={`Delete invoice ${r.id}`} tone="danger" onClick={() => notify.deleted(`Invoice ${r.id}`)} /></Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      </TableCard>
      <TableCard title="TableCard with no rows" count={0}>
        <CardBody>
          <EmptyState compact title="No invoices match" why="Clear the search to see every invoice." />
        </CardBody>
      </TableCard>
      <Panel title="StatGrid and Stat">
        <div className="space-y-3">
          <StatGrid label="Stats from items" items={[{ label: "Jobs", value: "1,204" }, { label: "Running", value: 3, tone: "warn" }, { label: "Failed", value: 2, tone: "bad", hint: "last hour" }, { label: "Succeeded", value: "1,199", tone: "ok" }]} />
          <StatGrid label="Stats from children" columns={3}>
            <Stat label="MRR" value="$4,210" hint="from 38 subscriptions" />
            <Stat label="Failed payments" value={2} tone="bad" onClick={() => notify.saved("Filter")} />
            <Stat label="Churn rate" value="1.2%" />
          </StatGrid>
        </div>
      </Panel>
      <Panel title="Facts, When and IdText">
        <div className="space-y-3">
          <Facts label="Function summary" items={[["Version", "Active v2 of 2"], { label: "Id", value: "fn_7f3a9c2e41", mono: true }, { label: "Deployed", value: <When value={ago(90)} /> }, false, ["Never run", <When value={null} fallback="Never" />]]} />
          <Facts label="Job details" columns={2} items={[["Queue", "default"], ["Created", <When value={ago(60 * 24 * 3)} />], ["Run at", <When value={ago(-20)} />], ["Day", <When value={ago(60 * 24 * 40)} day />], ["Id", <IdText id="job_01HXYZ7F3A9C2E41B" noun="job" />]]} />
          <p className="flex flex-wrap items-center gap-2 text-sm">
            Webhook URL <span className="font-mono text-xs">/billing/webhooks/stripe</span><CopyIcon text="/billing/webhooks/stripe" label="Copy webhook URL" />
            <CopyButton text="req-7f3a" label="Copy request id" iconOnly />
          </p>
        </div>
      </Panel>
      <Panel title="Badge, Dot, statusTone and Chips">
        <div className="space-y-3 text-sm">
          <div className="flex flex-wrap items-center gap-2">
            {STATUSES.map((s) => <Badge key={s} status={s} />)}
            <Badge tone="ok" dot>Active</Badge>
            <Badge tone="warn" title="Can also manage the server">Admin</Badge>
            <Badge>Custom</Badge>
          </div>
          <p className="flex flex-wrap items-center gap-4">
            {STATUSES.slice(0, 5).map((s) => <span key={s} className="inline-flex items-center gap-1.5"><Dot tone={statusTone(s)} pulse={s === "running"} />{s}</span>)}
          </p>
          <div className="flex flex-wrap items-center gap-4">
            <Chips items={["orders:read", "orders:write", "users:read", "storage:*"]} max={3} />
            <Chip mono={false}>recurring</Chip>
            <Chip tone="warn" mono={false}>Everything</Chip>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <ConfirmButton label="Archive price pro_month" short="Archive" consequence="New subscriptions can no longer use it." onConfirm={() => void notify.saved("Price pro_month")} />
            <IconButton icon={Delete02Icon} label="Delete row 3" tone="danger" />
          </div>
        </div>
      </Panel>
      <Panel title="Skeleton and Pager">
        <div className="space-y-4">
          <SkeletonTable rows={3} columns={4} label="Loading buckets" />
          <SkeletonCards count={3} label="Loading stats" />
          <Pager page={page} perPage={25} total={130} onPage={setPage} label="Records pages" />
          <Pager mode="more" shown={shown} hasMore={shown < 60} onMore={() => setShown((n) => n + 20)} noun="objects" />
        </div>
      </Panel>
      <CreateSheet
        open={creating}
        onOpenChange={setCreating}
        title="New thing"
        description="A CreateSheet holds the create form."
        submitLabel="Create thing"
        canSubmit={!validate.collectionName(name)}
        onSubmit={async () => ({ what: `Thing ${name}` })}
      >
        <FormField label="Name" error={touched ? validate.collectionName(name) : undefined}>
          {(p) => <Input {...p} value={name} onChange={(e) => setName(e.target.value)} onBlur={() => setTouched(true)} />}
        </FormField>
      </CreateSheet>
    </div>
  );
}
