/**
 * /_patterns (dev builds only): every shared component on one page, to check a change in both themes without
 * finding a screen that uses it. Not linked from the nav; App.tsx drops the route from production builds.
 * docs/ux/components.md explains which component to use for which job.
 */
import { Input } from "panelui-web/input";
import { useState } from "react";
import { ApiError } from "../api";
import { useCreateParam } from "../hooks";
import { notify } from "../toaster";
import { validate } from "../validate";
import { Button, CopyButton } from "./Button";
import { ConfirmDialog } from "./ConfirmDialog";
import { Check, Sel, Toggle } from "./controls";
import { CreateSheet } from "./CreateSheet";
import { EmptyState } from "./EmptyState";
import { ErrorPanel } from "./ErrorPanel";
import { FormField } from "./FormField";
import { HelpTip } from "./HelpTip";
import { PageHeader } from "./PageHeader";
import { Pager } from "./Pager";
import { SkeletonCards, SkeletonTable } from "./Skeleton";
import { Panel } from "./ui";
import { UseFromCode, UseFromCodeButton } from "./UseFromCode";

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
