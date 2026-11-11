import { useState } from "react";
import { useNavigate } from "react-router-dom";
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
import { Button, ErrorView } from "../../components/ui";
import { useLoad } from "../../hooks";
import { notify } from "../../toaster";
import { validate } from "../../validate";
import { OPTIONS, buildField, toRow, type OptValue, type Row } from "./schemaOptions";

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

  const patch = (i: number, p: Partial<Row>) => setRows((rs) => rs.map((r, j) => (j === i ? { ...r, ...p } : r)));
  const setOpt = (i: number, key: string, v: OptValue) => setRows((rs) => rs.map((r, j) => (j === i ? { ...r, opts: { ...r.opts, [key]: v } } : r)));

  const nameError = existing ? undefined : validate.collectionName(name);
  const rowError = (r: Row) => (r.base.system ? undefined : validate.fieldName(r.name));
  const duplicate = (r: Row, i: number) => (r.name && rows.findIndex((x) => x.name === r.name) !== i ? "Another field already has this name." : undefined);
  const invalid = !!nameError || rows.some((r, i) => rowError(r) || duplicate(r, i));

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
    }
  }

  return (
    <div className="max-w-5xl space-y-4">
      {!existing && <PageHeader title="New collection" description="A collection holds records that share a schema: its fields and their types." docs="docs/database.md" />}
      <div className="flex flex-wrap items-start gap-4">
        {!existing && (
          <FormField label="Name" required error={touched || name ? nameError : undefined} help="Letters, digits and underscores, starting with a letter.">
            {(p) => <Input {...p} size="sm" value={name} onChange={(e) => setName(e.target.value)} />}
          </FormField>
        )}
        <FormField label={<span className="inline-flex items-center gap-1">Type <HelpTip term="base" /> <HelpTip term="auth_collection" /> <HelpTip term="view" /></span>} help="Base: plain data. Auth: users who can sign in. View: a read-only query.">
          {(p) => <Sel {...p} aria-label="Type" value={kind} onChange={setKind} options={[{ value: "base", label: "Base" }, { value: "auth", label: "Auth" }, { value: "view", label: "View" }]} />}
        </FormField>
        <FormField label="Schema mode" term="schema_mode" help="Strict rejects unknown fields. Flexible keeps extra fields.">
          {(p) => <Sel {...p} aria-label="Schema mode" value={mode} onChange={setMode} options={[{ value: "strict", label: "Strict" }, { value: "flexible", label: "Flexible" }]} />}
        </FormField>
        <div className="pt-6">
          <Check checked={versioned} onChange={setVersioned} label="Keep record versions" description="Every change is kept so it can be restored." />
        </div>
      </div>

      <h2 className="text-sm font-medium">Fields</h2>
      {rows.length === 0 && <EmptyState compact title="No fields yet" why="Fields are the typed columns of each record, such as a title (text) or a price (number). Add the first one below." />}
      {rows.length > 0 && (
        <div className="overflow-x-auto">
          <Table className="w-full text-left text-sm" aria-label="Fields">
            <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground">
              <Table.Row><Table.Head className="py-1">Field</Table.Head><Table.Head>Type</Table.Head><Table.Head>Required</Table.Head><Table.Head>Unique</Table.Head><Table.Head>Options</Table.Head><Table.Head><span className="sr-only">Actions</span></Table.Head></Table.Row>
            </Table.Header>
            <Table.Body>
              {rows.map((r, i) => {
                const locked = !!r.base.system;
                const err = touched || r.name ? (rowError(r) ?? duplicate(r, i)) : undefined;
                return (
                  <Table.Row key={r.base.id ?? `new-${i}`} className="border-t border-border align-top">
                    <Table.Cell className="py-2 pr-2">
                      <Input size="sm" aria-label={`Field ${i + 1} name`} aria-invalid={err ? true : undefined} disabled={locked} className="w-full" value={r.name} onChange={(e) => patch(i, { name: e.target.value })} />
                      {err && <p className="mt-1 text-xs font-medium text-destructive">{err}</p>}
                    </Table.Cell>
                    <Table.Cell className="py-2 pr-2">
                      <Sel aria-label={`Field ${i + 1} type`} disabled={locked} value={r.type} onChange={(v) => patch(i, { type: v, opts: {} })} options={[...new Set([...FIELD_TYPES, r.type])].map((t) => ({ value: t, label: t }))} />
                    </Table.Cell>
                    <Table.Cell className="py-2"><Check aria-label={`Field ${i + 1} required`} disabled={locked} checked={r.required} onChange={(v) => patch(i, { required: v })} /></Table.Cell>
                    <Table.Cell className="py-2"><Check aria-label={`Field ${i + 1} unique`} disabled={locked} checked={r.unique} onChange={(v) => patch(i, { unique: v })} /></Table.Cell>
                    <Table.Cell className="py-2 pr-2">
                      <div className="flex flex-wrap items-end gap-2">
                        {(OPTIONS[r.type] ?? []).map((o) => {
                          const label = `Field ${i + 1} ${o.label.toLowerCase()}`;
                          if (o.kind === "bool") return <Check key={o.key} aria-label={label} label={o.label} disabled={locked} checked={r.opts[o.key] === true} onChange={(v) => setOpt(i, o.key, v)} />;
                          if (o.kind === "collection") {
                            return (
                              <label key={o.key} className="flex flex-col gap-0.5 text-xs text-muted-foreground">{o.label}
                                <Sel aria-label={label} disabled={locked} value={String(r.opts[o.key] ?? "")} onChange={(v) => setOpt(i, o.key, v)} placeholder="Choose a collection…" options={(all.data ?? []).filter((c) => c.id).map((c) => ({ value: c.id!, label: c.name }))} />
                              </label>
                            );
                          }
                          return (
                            <label key={o.key} className="flex flex-col gap-0.5 text-xs text-muted-foreground">{o.label}
                              <Input size="sm" aria-label={label} disabled={locked} className={o.kind === "list" ? "w-44" : "w-24"} inputMode={o.kind === "list" ? undefined : "decimal"} placeholder={o.help} value={String(r.opts[o.key] ?? "")} onChange={(e) => setOpt(i, o.key, e.target.value)} />
                            </label>
                          );
                        })}
                        {(OPTIONS[r.type] ?? []).length === 0 && <span className="text-xs text-muted-foreground">None for this type</span>}
                      </div>
                    </Table.Cell>
                    <Table.Cell className="py-2">{locked ? <span className="text-xs text-muted-foreground">system</span> : <Button onClick={() => setRows((rs) => rs.filter((_, j) => j !== i))} aria-label={`Remove field ${i + 1}`}>Remove</Button>}</Table.Cell>
                  </Table.Row>
                );
              })}
            </Table.Body>
          </Table>
        </div>
      )}
      <Button onClick={() => setRows((rs) => [...rs, toRow({ name: "", type: "text" })])}>Add field</Button>

      <ErrorView error={problem} />
      <div className="flex gap-2">
        <Button tone="primary" disabled={touched && invalid} onClick={() => void save()}>{existing ? "Save schema" : "Create collection"}</Button>
        {existing && !existing.system && (
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
        )}
      </div>
    </div>
  );
}
