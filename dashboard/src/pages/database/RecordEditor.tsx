/**
 * RecordEditor: the side sheet that adds or edits one record (audit A-07, A-11). A typed form by default, with an
 * "Edit as JSON" switch for power users. Opened from the grid by `?record=new` or `?record=<id>`.
 * Replacing an existing record sends `If-Match` with the version that was loaded.
 */
import { useState } from "react";
import { Textarea } from "panelui-web/textarea";
import { createRecord, getRecord, replaceRecord } from "../../api";
import type { CollectionDef, RecordObject } from "../../api";
import { CreateSheet, DetailSheet } from "../../components/CreateSheet";
import { Toggle } from "../../components/controls";
import { ErrorPanel, fieldErrors } from "../../components/ErrorPanel";
import { UseFromCode } from "../../components/UseFromCode";
import { CopyButton } from "../../components/ui";
import { when } from "../functions/util";
import { record as recordSnippet, serverUrl } from "../../snippets";
import { useLoad } from "../../hooks";
import { notify } from "../../toaster";
import { SkeletonLines } from "../../components/Skeleton";
import RecordForm from "./RecordForm";
import { parseJsonBody, toBody, toDraft, type Draft } from "./recordDraft";

export interface RecordEditorProps {
  collection: CollectionDef;
  collections: CollectionDef[];
  /** `new`, or the id of the record to edit. */
  recordId: string;
  onClose: () => void;
  /** Called after a successful save so the list reloads. */
  onSaved: () => void;
}

export default function RecordEditor({ collection, collections, recordId, onClose, onSaved }: RecordEditorProps) {
  const isNew = recordId === "new";
  const load = useLoad((s) => (isNew ? Promise.resolve(undefined) : getRecord(collection.name, recordId, s)), `${collection.name}/${recordId}`);
  if (load.error || load.loading) {
    return (
      <DetailSheet open onOpenChange={(o) => !o && onClose()} title={`Record ${recordId}`}>
        {load.error ? <ErrorPanel error={load.error} onRetry={load.reload} /> : <SkeletonLines lines={4} label="Loading record" />}
      </DetailSheet>
    );
  }
  return <Editor key={recordId} collection={collection} collections={collections} recordId={recordId} record={load.data} onClose={onClose} onSaved={onSaved} />;
}

function Editor({ collection, collections, recordId, record, onClose, onSaved }: Omit<RecordEditorProps, never> & { record?: RecordObject }) {
  const isNew = recordId === "new";
  const [asJson, setAsJson] = useState(false);
  const [draft, setDraft] = useState<Draft>(() => toDraft(collection.fields, record));
  const [jsonText, setJsonText] = useState("");
  const [jsonError, setJsonError] = useState<string>();
  const [tried, setTried] = useState(false);
  const [saveError, setSaveError] = useState<unknown>();

  const built = toBody(collection.fields, draft);
  const errors = { ...(tried ? built.errors : {}), ...fieldErrors(saveError) };
  const change = (name: string, v: string | boolean) => {
    setDraft((d) => ({ ...d, [name]: v }));
    setSaveError(undefined);
  };

  function switchMode(json: boolean) {
    if (json) {
      setJsonText(JSON.stringify(built.body, null, 2));
      setJsonError(undefined);
      setAsJson(true);
      return;
    }
    try {
      const body = parseJsonBody(jsonText);
      setDraft(toDraft(collection.fields, { ...(record ?? {}), ...body } as RecordObject));
      setJsonError(undefined);
      setAsJson(false);
    } catch (e) {
      setJsonError((e as Error).message);
    }
  }

  return (
    <CreateSheet
      open
      onOpenChange={(o) => !o && onClose()}
      title={isNew ? "New record" : `Edit record ${recordId}`}
      description={isNew ? `Adds a record to ${collection.name}.` : `Collection ${collection.name}${record?._version ? `, version ${record._version}` : ""}.`}
      submitLabel={isNew ? "Create record" : "Save record"}
      onError={setSaveError}
      onSubmit={async () => {
        setTried(true);
        let body: Record<string, unknown>;
        if (asJson) {
          try {
            body = parseJsonBody(jsonText);
          } catch (e) {
            setJsonError((e as Error).message);
            throw e;
          }
        } else {
          if (Object.keys(built.errors).length > 0) throw new Error("Some fields need attention.");
          body = built.body;
        }
        if (isNew) await createRecord(collection.name, body);
        else {
          await replaceRecord(collection.name, recordId, body, record?._version);
          notify.saved("Record");
        }
        onSaved();
        return isNew ? { what: "Record" } : undefined;
      }}
    >
      {!isNew && record && (
        <dl aria-label="Record facts" className="grid grid-cols-[5rem_minmax(0,1fr)] items-center gap-x-3 gap-y-1.5 rounded-lg border border-border bg-muted/30 px-3 py-2.5 text-xs">
          <dt className="text-muted-foreground">id</dt>
          <dd className="flex min-w-0 items-center justify-between gap-2"><span className="truncate font-mono" title={recordId}>{recordId}</span><CopyButton text={recordId} label="Copy id" className="h-6 px-2 text-xs" /></dd>
          {typeof record.created === "string" && <><dt className="text-muted-foreground">Created</dt><dd className="tabular-nums">{when(record.created)}</dd></>}
          {typeof record.updated === "string" && <><dt className="text-muted-foreground">Updated</dt><dd className="tabular-nums">{when(record.updated)}</dd></>}
        </dl>
      )}
      <Toggle checked={asJson} onChange={switchMode} label="Edit as JSON" description="Type the whole record as a JSON object." />
      {jsonError && <p role="alert" className="text-xs font-medium text-destructive">{jsonError}</p>}
      {asJson ? (
        <Textarea size="sm" autoResize={false} spellCheck={false} aria-label="Record JSON" className="h-96 font-mono" value={jsonText} onChange={(e) => { setJsonText(e.target.value); setJsonError(undefined); }} />
      ) : (
        <RecordForm collection={collection} collections={collections} draft={draft} onChange={change} errors={errors} />
      )}
      {!isNew && (
        <details className="rounded-lg border border-border px-3 py-2 text-sm">
          <summary className="cursor-pointer font-medium">Use from code</summary>
          <div className="mt-2">
            <UseFromCode snippet={recordSnippet(serverUrl(), collection.name, recordId)} title={null} />
          </div>
        </details>
      )}
    </CreateSheet>
  );
}
