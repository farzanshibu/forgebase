import { Building03Icon } from "@hugeicons/core-free-icons";
import { Input } from "panelui-web/input";
import { Table } from "panelui-web/table";
import { useState } from "react";
import { Link, Navigate, Route, Routes, useLocation, useNavigate, useParams } from "react-router-dom";
import { createOrg, deleteOrg, getOrg, listOrgs, renameOrg } from "../../api";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { RouteTabs } from "../../components/controls";
import { CreateSheet } from "../../components/CreateSheet";
import { useCrumbs } from "../../components/crumbs";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { FormField } from "../../components/FormField";
import { HelpTip } from "../../components/HelpTip";
import { SkeletonLines, SkeletonTable } from "../../components/Skeleton";
import { Button } from "../../components/ui";
import { fieldErrors } from "../../errors";
import { useLoad } from "../../hooks";
import { notify } from "../../toaster";
import { Initials, Modal } from "../auth/common";
import AclPanel from "./AclPanel";
import { InvitationsTab, MembersTab, OverridesTab, ProjectsTab, TeamsTab } from "./OrgTabs";
import RolesPanel from "./RolesPanel";
import RoleSetsPanel from "./RoleSetsPanel";
import { ActionError } from "./shared";
import { useRun } from "./util";
import { CardBody, HEAD_ROW, TableCard, Th } from "../../components/TableCard";
import { Chip } from "../../components/Chips";
import { IdText } from "../../components/IdText";

const SLUG = /^[a-z0-9][a-z0-9-]*$/;
const slugError = (s: string) => (!s ? "Enter a slug." : SLUG.test(s) ? undefined : "Use lower-case letters, digits and dashes, starting with a letter or digit.");
const orgHref = (id: string) => `/access/orgs/${encodeURIComponent(id)}`;

export function OrgList() {
  const orgs = useLoad(listOrgs, "orgs");
  const [creating, setCreating] = useState(false);
  const newButton = <Button tone="primary" onClick={() => setCreating(true)}>New organization</Button>;
  return (
    <>
      <TableCard
        title="Organizations"
        count={orgs.data?.length}
        description="An organization groups your customers' or your team's members, projects, teams and roles."
        actions={orgs.data?.length ? newButton : undefined}
      >
        {!!orgs.error && <CardBody><ErrorPanel error={orgs.error} onRetry={orgs.reload} /></CardBody>}
        {orgs.loading && !orgs.data && !orgs.error && <CardBody><SkeletonTable rows={3} columns={3} label="Loading organizations" /></CardBody>}
        {orgs.data?.length === 0 && (
          <CardBody>
            <EmptyState
              title="No organizations yet"
              why="An organization groups your team's members, projects and roles, so each customer or team only sees its own."
              action={newButton}
              icon={Building03Icon}
              docs="docs/adr/0008-policy.md"
            />
          </CardBody>
        )}
        {!!orgs.data?.length && (
          <Table framed className="text-sm" aria-label="Organizations">
            <Table.Header className={HEAD_ROW}>
              <Table.Row>
                <Th>Organization</Th>
                <Th><span className="inline-flex items-center gap-1">Realm <HelpTip term="realm" /></span></Th>
                <Th>Id</Th>
              </Table.Row>
            </Table.Header>
            <Table.Body>
              {orgs.data.map((o) => (
                <Table.Row key={o.id}>
                  <Table.Cell>
                    <span className="flex min-w-0 items-center gap-2.5">
                      <Initials name={o.name} shape="square" />
                      <span className="flex min-w-0 flex-col">
                        <Link className="truncate font-medium hover:text-primary hover:underline" to={orgHref(o.id)}>{o.name}</Link>
                        <code className="truncate font-mono text-xs text-muted-foreground">{o.slug}</code>
                      </span>
                    </span>
                  </Table.Cell>
                  <Table.Cell><Chip>{o.realm_id}</Chip></Table.Cell>
                  <Table.Cell className="max-w-48"><IdText id={o.id} what="organization id" /></Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
      {creating && <NewOrgSheet onClose={() => setCreating(false)} onCreated={orgs.reload} />}
    </>
  );
}

function NewOrgSheet({ onClose, onCreated }: { onClose: () => void; onCreated: () => void }) {
  const [slug, setSlug] = useState("");
  const [name, setName] = useState("");
  const [touched, setTouched] = useState(false);
  const [saveError, setSaveError] = useState<unknown>();
  const byField = fieldErrors(saveError);
  return (
    <CreateSheet
      open
      onOpenChange={(v) => { if (!v) onClose(); }}
      title="New organization"
      description="Members, projects, teams and roles live inside it."
      submitLabel="Create organization"
      canSubmit={!slugError(slug)}
      onError={setSaveError}
      onSubmit={async () => {
        const o = await createOrg(slug, name.trim() || slug);
        onCreated();
        return { what: `Organization ${o.name}`, href: orgHref(o.id) };
      }}
    >
      <FormField label="Name" help="Shown in lists. Leave empty to use the slug." error={byField.name}>
        {(p) => <Input {...p} className="w-full" value={name} placeholder="Acme Inc" onChange={(e) => {
          const v = e.target.value;
          setName(v);
          if (!touched) setSlug(v.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, ""));
        }} />}
      </FormField>
      <FormField label="Slug" required help="Lower-case letters, digits and dashes. Used in URLs and the API." error={(touched ? slugError(slug) : undefined) ?? byField.slug}>
        {(p) => <Input {...p} className="w-full font-mono" value={slug} placeholder="acme" autoComplete="off" spellCheck={false} onChange={(e) => { setSlug(e.target.value); setTouched(true); }} />}
      </FormField>
    </CreateSheet>
  );
}

const TABS = [
  ["projects", "Projects"], ["members", "Members"], ["teams", "Teams"], ["roles", "Roles"], ["role-sets", "Role sets"],
  ["invitations", "Invitations"], ["overrides", "Overrides"], ["acl", "Resource ACL"],
] as const;

export function OrgDetail() {
  const { org = "" } = useParams();
  const nav = useNavigate();
  const { pathname } = useLocation();
  const o = useLoad((s) => getOrg(org, s), `org:${org}`);
  const [renaming, setRenaming] = useState(false);
  const base = orgHref(org);
  const tab = TABS.find(([p]) => pathname.startsWith(`${base}/${p}`));
  useCrumbs(o.data ? [
    { label: "Access", to: "/access" },
    { label: "Organizations", to: "/access" },
    { label: o.data.name, to: base },
    ...(tab ? [{ label: tab[1] }] : []),
  ] : undefined);

  if (o.error) return <ErrorPanel error={o.error} onRetry={o.reload} />;
  if (!o.data) return <SkeletonLines lines={3} label="Loading organization" />;
  const d = o.data;
  return (
    <div className="space-y-5">
      <header className="flex flex-wrap items-start justify-between gap-4">
        <div className="flex min-w-0 items-center gap-3">
          <Initials name={d.name} size="lg" shape="square" />
          <div className="min-w-0">
            <h2 className="truncate text-xl font-semibold tracking-tight">{d.name}</h2>
            <p className="mt-0.5 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
              <span>Slug <code className="font-mono text-foreground/80">{d.slug}</code></span>
              <span className="inline-flex items-center gap-1">Realm <code className="font-mono text-foreground/80">{d.realm_id}</code></span>
              <IdText id={d.id} what="organization id" />
            </p>
          </div>
        </div>
        <div className="flex items-center gap-2">
          <Button onClick={() => setRenaming(true)}>Rename</Button>
          <ConfirmDialog
            trigger={<Button tone="danger">Delete organization</Button>}
            title={`Delete organization ${d.name}?`}
            consequence="Its projects, members, teams and roles are removed. Apps that use them lose access."
            confirmLabel="Delete organization"
            confirmText={d.slug}
            onConfirm={async () => { await deleteOrg(org); notify.deleted(`Organization ${d.name}`); nav("/access"); }}
          />
        </div>
      </header>
      <RouteTabs label="Organization sections" tabs={TABS.map(([p, label]) => ({ to: `${base}/${p}`, label }))} />
      <Routes>
        <Route index element={<Navigate to="projects" replace />} />
        <Route path="projects" element={<ProjectsTab org={org} />} />
        <Route path="members" element={<MembersTab org={org} />} />
        <Route path="teams" element={<TeamsTab org={org} />} />
        <Route path="roles" element={<RolesPanel org={org} />} />
        <Route path="role-sets" element={<RoleSetsPanel org={org} />} />
        <Route path="invitations" element={<InvitationsTab org={org} />} />
        <Route path="overrides" element={<OverridesTab org={org} />} />
        <Route path="acl" element={<AclPanel />} />
      </Routes>
      {renaming && <RenameDialog current={d.name} org={org} onClose={() => setRenaming(false)} onDone={o.reload} />}
    </div>
  );
}

function RenameDialog({ current, org, onClose, onDone }: { current: string; org: string; onClose: () => void; onDone: () => void }) {
  const [name, setName] = useState(current);
  const { run, error, busy } = useRun();
  const save = () => void run(async () => {
    await renameOrg(org, name.trim());
    notify.saved("Organization name");
    onDone();
    onClose();
  });
  const can = !!name.trim() && name.trim() !== current && !busy;
  return (
    <Modal
      title="Rename organization"
      description="Only the display name changes. The slug and id stay the same, so links and API calls keep working."
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button tone="primary" disabled={!can} onClick={save}>{busy ? "Saving…" : "Save name"}</Button>
        </>
      }
    >
      <form className="space-y-3" onSubmit={(e) => { e.preventDefault(); if (can) save(); }}>
        <FormField label="New name">
          {(p) => <Input {...p} className="w-full" value={name} autoFocus onChange={(e) => setName(e.target.value)} />}
        </FormField>
        <ActionError error={error} />
      </form>
    </Modal>
  );
}
