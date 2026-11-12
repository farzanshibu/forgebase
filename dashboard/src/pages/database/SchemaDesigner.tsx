import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { Database01Icon, Delete02Icon, LockKeyIcon, PlusSignIcon, UserGroupIcon, ViewIcon } from "@hugeicons/core-free-icons";
import { Badge } from "panelui-web/badge";
import { Input } from "panelui-web/input";
import { Table } from "panelui-web/table";
import { createCollection, deleteCollection, listCollections, updateCollection, FIELD_TYPES } from "../../api";
import type { CollectionDef, FieldDef } from "../../api";
import { Check, Sel } from "../../components/controls";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { EmptyState } from "../../components/EmptyState";
import { FormField } from "../../components/FormField";
import { HelpTip } from "../../components/HelpTip";
import { PageHeader } from "../../components/PageHeader";
import { SaveBar } from "../../components/SaveBar";
import { Button, ErrorView } from "../../components/ui";
import type { GlossaryTerm } from "../../glossary";
import { useLoad } from "../../hooks";
import { notify } from "../../toaster";
import { validate } from "../../validate";
import { Icon, IconButton, TableCard } from "./parts";
import { OPTIONS, buildField, toRow, type OptValue, type Row } from "./schemaOptions";

const KINDS: { value: string; label: string; text: string; term: GlossaryTerm; icon: typeof Database01Icon }[] = [
  { value: "base", label: "Base", text: "Plain data, such as products or orders.", term: "base", icon: Database01Icon },
  { value: "auth", label: "Auth", text: "Users who can sign in, with their own fields.", term: "auth_collection", icon: UserGroupIcon },
  { value: "view", label: "View", text: "A read-only query over other collections.", term: "view", icon: ViewIcon },
];

/** A titled block of the form: heading and one line on the left, the controls on the right (stacked below on narrow screens). */
function Section({ title, text, children }: { title: string; text: string; children: React.ReactNode }) {
  return (
    <section aria-label={title} className="grid gap-4 rounded-xl border border-border bg-card p-5 xl:grid-cols-[14rem_1fr] xl:gap-8">
      <div>
        <h2 className="text-sm font-medium">{title}</h2>
        <p className="mt-1 text-xs text-muted-foreground">{text}</p>
      </div>
      <div className="min-w-0 space-y-5">{children}</div>
    </section>
  );
}

export default function SchemaDesigner({ existing, onSaved }: { existing?: CollectionDef; onSaved?: () => void }) {
  const nav = useNavigate();
  const all = useLoad(listCollections, "collections");
  const [name, setName] = useState(existing?.name ?? "");
  const [kind, setKind] = useState(existing?.type ?? "base");
  const [mode, setMode] = useState(existing?.schema_mode ?? "strict");
  const [versioned, setVersioned] = useState(!!existing?.versioned);
  const [rows, setRows] = useState<Row[]>((existing?.fields ?? []).map(toRow));
  const [problem, setProblem] = useState<unknown>();
  const [touched, setTouched] = useState(false);
  const [busy, setBusy] = useState(false);

  const patch = (i: number, p: Partial<Row>) => setRows((rs) => rs.map((r, j) => (j === i ? { ...r, ...p } : r)));
  const setOpt = (i: number, key: string, v: OptValue) => setRows((rs) => rs.map((r, j) => (j === i ? { ...r, opts: { ...r.opts, [key]: v } } : r)));

  const nameError = existing ? undefined : validate.collectionName(name);
  const rowError = (r: Row) => (r.base.system ? undefined : validate.fieldName(r.name));
  const duplicate = (r: Row, i: number) => (r.name && rows.findIndex((x) => x.name === r.name) !== i ? "Another field already has this name." : undefined);
  const invalid = !!nameError || rows.some((r, i) => rowError(r) || duplicate(r, i));
  const userFields = rows.filter((r) => !r.base.system).length;

  function build(): CollectionDef | null {
    const fields: FieldDef[] = [];
    for (const r of rows) {
      if (r.base.system) {
        fields.push(r.base);
        continue;
      }
      const b = buildField(r);
      if (b.error) {
        setProblem(new Error(b.error));
        return null;
      }
      fields.push(b.field!);
    }
    return { ...(existing ?? {}), name, type: kind, schema_mode: mode, versioned, fields };
  }

  async function save() {
    setTouched(true);
    if (invalid) return;
    const def = build();
    if (!def) return;
    setBusy(true);
    try {
      if (existing) {
        await updateCollection(existing.name, def);
        setProblem(undefined);
        notify.saved("Schema");
        onSaved?.();
      } else {
        await createCollection(def);
        notify.created(`Collection ${name}`);
        nav(`/database/c/${encodeURIComponent(name)}`);
      }
    } catch (e) {
      setProblem(e);
    } finally {
      setBusy(false);
    }
  }

  const addField = () => setRows((rs) => [...rs, toRow({ name: "", type: "text" })]);

  return (
    <div className="max-w-6xl space-y-6">
      {!existing && <PageHeader title="New collection" description="A collection holds records that share a schema: its fields and their types." docs="docs/database.md" />}

      <Section title="Collection" text={existing ? "What kind of collection this is and how strictly records follow the schema." : "Name it, pick what kind of data it holds, and how strictly records follow the schema."}>
        {!existing && (
          <FormField label="Name" required error={touched || name ? nameError : undefined} help="Letters, digits and underscores, starting with a letter. Apps use it in URLs." className="max-w-md">
            {(p) => <Input {...p} size="sm" placeholder="posts" className="font-mono" value={name} onChange={(e) => setName(e.target.value)} />}
          </FormField>
        )}
        <fieldset>
          <legend className="mb-2 text-sm font-medium">Type</legend>
          <div className="grid gap-2 sm:grid-cols-3">
            {KINDS.map((k) => (
              <div key={k.value} className="relative">
                <label className="flex h-full cursor-pointer flex-col gap-1 rounded-lg border border-border p-3 pr-9 transition-colors hover:bg-muted/40 has-[:checked]:border-primary has-[:checked]:bg-primary/5 has-[:focus-visible]:ring-2 has-[:focus-visible]:ring-ring">
                  <span className="flex items-center gap-2 text-sm font-medium">
                    <input type="radio" name="collection-type" value={k.value} checked={kind === k.value} onChange={() => setKind(k.value)} className="peer sr-only" />
                    <span aria-hidden="true" className="size-3.5 shrink-0 rounded-full border border-input bg-background transition-[border-width] peer-checked:border-4 peer-checked:border-primary" />
                    <Icon icon={k.icon} size={15} className="text-muted-foreground" />
                    {k.label}
                  </span>
                  <span className="text-xs text-muted-foreground">{k.text}</span>
                </label>
                <span className="absolute right-3 top-3.5"><HelpTip term={k.term} /></span>
              </div>
            ))}
          </div>
        </fieldset>
        <div className="flex flex-wrap items-start gap-x-8 gap-y-4">
          <FormField label="Schema mode" term="schema_mode" help="Strict rejects unknown fields. Flexible keeps extra fields.">
            {(p) => <Sel {...p} aria-label="Schema mode" className="w-44" value={mode} onChange={setMode} options={[{ value: "strict", label: "Strict" }, { value: "flexible", label: "Flexible" }]} />}
          </FormField>
          <div className="pt-6">
            <Check checked={versioned} onChange={setVersioned} label="Keep record versions" description="Every change is kept so it can be restored." />
          </div>
        </div>
      </Section>

      <TableCard
        title="Fields"
        count={userFields}
        description="The typed columns of each record. ForgeBase adds id, created and updated for you."
        footer={
          <Button onClick={addField}>
            <Icon icon={PlusSignIcon} size={14} />
            Add field
          </Button>
        }
      >
        {rows.length === 0 && (
          <div className="p-4">
            <EmptyState compact title="No fields yet" why="Fields are the typed columns of each record, such as a title (text) or a price (number). Use Add field to create the first one." />
          </div>
        )}
        {rows.length > 0 && (
          <Table framed aria-label="Fields">
            <Table.Header className="bg-muted/40">
              <Table.Row>
                <Table.Head className="w-48">Field</Table.Head>
                <Table.Head className="w-36">Type</Table.Head>
                <Table.Head align="center" className="w-16">Required</Table.Head>
                <Table.Head align="center" className="w-16">Unique</Table.Head>
                <Table.Head>Options</Table.Head>
                <Table.Head align="end" className="w-12"><span className="sr-only">Actions</span></Table.Head>
              </Table.Row>
            </Table.Header>
            <Table.Body>
              {rows.map((r, i) => {
                const locked = !!r.base.system;
                const err = touched || r.name ? (rowError(r) ?? duplicate(r, i)) : undefined;
                const opts = OPTIONS[r.type] ?? [];
                return (
                  <Table.Row key={r.base.id ?? `new-${i}`} className={`align-top ${locked ? "bg-muted/20" : ""}`}>
                    <Table.Cell className="py-2.5">
                      <Input size="sm" aria-label={`Field ${i + 1} name`} aria-invalid={err ? true : undefined} disabled={locked} placeholder="field_name" className="w-full font-mono" value={r.name} onChange={(e) => patch(i, { name: e.target.value })} />
                      {err && <p className="mt-1 text-xs font-medium text-destructive">{err}</p>}
                    </Table.Cell>
                    <Table.Cell className="py-2.5">
                      <Sel aria-label={`Field ${i + 1} type`} className="w-full" disabled={locked} value={r.type} onChange={(v) => patch(i, { type: v, opts: {} })} options={[...new Set([...FIELD_TYPES, r.type])].map((t) => ({ value: t, label: t }))} />
                    </Table.Cell>
                    <Table.Cell align="center" className="py-2.5"><span className="inline-flex h-8 items-center"><Check aria-label={`Field ${i + 1} required`} disabled={locked} checked={r.required} onChange={(v) => patch(i, { required: v })} /></span></Table.Cell>
                    <Table.Cell align="center" className="py-2.5"><span className="inline-flex h-8 items-center"><Check aria-label={`Field ${i + 1} unique`} disabled={locked} checked={r.unique} onChange={(v) => patch(i, { unique: v })} /></span></Table.Cell>
                    <Table.Cell className="py-2.5">
                      <div className="flex flex-wrap items-end gap-x-3 gap-y-2">
                        {opts.map((o) => {
                          const label = `Field ${i + 1} ${o.label.toLowerCase()}`;
                          if (o.kind === "bool") return <span key={o.key} className="inline-flex h-8 items-center"><Check aria-label={label} label={o.label} disabled={locked} checked={r.opts[o.key] === true} onChange={(v) => setOpt(i, o.key, v)} /></span>;
                          if (o.kind === "collection") {
                            return (
                              <label key={o.key} className="flex flex-col gap-1 text-xs text-muted-foreground">{o.label}
                                <Sel aria-label={label} className="w-48" disabled={locked} value={String(r.opts[o.key] ?? "")} onChange={(v) => setOpt(i, o.key, v)} placeholder="Choose a collection…" options={(all.data ?? []).filter((c) => c.id).map((c) => ({ value: c.id!, label: c.name }))} />
                              </label>
                            );
                          }
                          return (
                            <label key={o.key} className="flex flex-col gap-1 text-xs text-muted-foreground">{o.label}
                              <Input size="sm" aria-label={label} disabled={locked} className={o.kind === "list" ? "w-48" : "w-24"} inputMode={o.kind === "list" ? undefined : "decimal"} placeholder={o.help ?? "any"} value={String(r.opts[o.key] ?? "")} onChange={(e) => setOpt(i, o.key, e.target.value)} />
                            </label>
                          );
                        })}
                        {opts.length === 0 && <span className="inline-flex h-8 items-center text-xs text-muted-foreground">No options for this type</span>}
                      </div>
                    </Table.Cell>
                    <Table.Cell align="end" className="py-2.5">
                      {locked
                        ? <span className="inline-flex h-8 items-center" title="ForgeBase manages this field"><Badge variant="outline" size="sm"><Icon icon={LockKeyIcon} size={12} />system</Badge></span>
                        : <IconButton icon={Delete02Icon} tone="danger" label={`Remove field ${i + 1}`} onClick={() => setRows((rs) => rs.filter((_, j) => j !== i))} />}
                    </Table.Cell>
                  </Table.Row>
                );
              })}
            </Table.Body>
          </Table>
        )}
      </TableCard>

      <ErrorView error={problem} />
      <SaveBar label={existing ? "Save schema" : "Create collection"}>
        <p className="text-xs text-muted-foreground">
          {touched && invalid ? <span className="font-medium text-destructive">Fix the highlighted fields to continue.</span> : existing ? "Changes apply when you save. Existing records keep their data." : `${userFields} ${userFields === 1 ? "field" : "fields"} so far. You can change them later.`}
        </p>
        <div className="flex gap-2">
          {!existing && <Button onClick={() => nav("/database")}>Cancel</Button>}
          <Button tone="primary" disabled={busy || (touched && invalid)} onClick={() => void save()}>
            {existing ? (busy ? "Saving…" : "Save schema") : busy ? "Creating…" : "Create collection"}
          </Button>
        </div>
      </SaveBar>

      {existing && !existing.system && (
        <section aria-label="Danger zone" className="flex flex-wrap items-center justify-between gap-4 rounded-xl border border-destructive/40 p-5">
          <div>
            <h2 className="text-sm font-medium text-destructive">Delete this collection</h2>
            <p className="mt-1 text-xs text-muted-foreground">Removes its schema and every record. This cannot be undone.</p>
          </div>
          <ConfirmDialog
            trigger={<Button tone="danger">Delete collection</Button>}
            title={`Delete collection ${existing.name}?`}
            consequence="Every record in it and its schema are deleted. Apps that read it get 404. This cannot be undone."
            confirmLabel="Delete collection"
            confirmText={existing.name}
            onConfirm={async () => {
              await deleteCollection(existing.name);
              notify.deleted(`Collection ${existing.name}`);
              nav("/database");
            }}
          />
        </section>
      )}
    </div>
  );
}
