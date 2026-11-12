import { useState } from "react";
import { Input } from "panelui-web/input";
import { Textarea } from "panelui-web/textarea";
import { VISIBILITIES, bucketUsage, updateBucket } from "../../api";
import type { Bucket, Visibility } from "../../api";
import { Check, Sel } from "../../components/controls";
import { FormField } from "../../components/FormField";
import { Button, ErrorView } from "../../components/ui";
import { formatBytes } from "../../format";
import { useLoad } from "../../hooks";
import { notify } from "../../toaster";
import { VISIBILITY_LOOK, parseBytes, parseTypes } from "./util";

export default function BucketSettings({ bucket, onSaved }: { bucket: Bucket; onSaved: () => void }) {
  const [visibility, setVisibility] = useState<Visibility>(bucket.visibility);
  const [versioning, setVersioning] = useState(bucket.versioning);
  const [max, setMax] = useState(bucket.max_object_bytes ? String(bucket.max_object_bytes) : "");
  const [types, setTypes] = useState((bucket.allowed_content_types ?? []).join("\n"));
  const [error, setError] = useState<unknown>();
  const [maxError, setMaxError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const usage = useLoad((s) => bucketUsage(bucket.name, s), `bucket-usage:${bucket.name}`);
  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    setError(undefined);
    setMaxError(undefined);
    let bytes: number | null;
    try { bytes = parseBytes(max); } catch (err) { setMaxError((err as Error).message); return; }
    setBusy(true);
    try {
      await updateBucket(bucket.name, { visibility, versioning, max_object_bytes: bytes, allowed_content_types: parseTypes(types) });
      notify.saved("Bucket policy");
      onSaved();
    } catch (err) { setError(err); } finally { setBusy(false); }
  };
  return (
    <form onSubmit={(e) => void save(e)} noValidate aria-label="Bucket policy" className="max-w-3xl overflow-hidden rounded-xl border border-border bg-card">
      <header className="border-b border-border px-5 py-3">
        <h2 className="text-sm font-medium">Bucket policy</h2>
        <p className="mt-0.5 text-xs text-muted-foreground">
          Who can read the bucket, whether it keeps history, and what it accepts.
          {usage.data && (
            <span aria-label="Bucket space" className="mt-1 block">
              Uses {formatBytes(usage.data.logical_bytes)} in {usage.data.objects} objects; {formatBytes(usage.data.version_bytes)} across {usage.data.versions} versions.
            </span>
          )}
        </p>
      </header>
      <div className="divide-y divide-border text-sm">
        <div className="grid gap-3 px-5 py-4 sm:grid-cols-[12rem_1fr]">
          <div><p className="font-medium">Access</p><p className="text-xs text-muted-foreground">Rules and the access list refine it.</p></div>
          <FormField label="Visibility" help={VISIBILITY_LOOK[visibility].text + "."}>
            {(p) => <Sel {...p} aria-label="Visibility" className="w-56" value={visibility} onChange={(v) => setVisibility(v as Visibility)} options={VISIBILITIES.map((v) => ({ value: v, label: v }))} />}
          </FormField>
        </div>
        <div className="grid gap-3 px-5 py-4 sm:grid-cols-[12rem_1fr]">
          <div><p className="font-medium">History</p><p className="text-xs text-muted-foreground">Earlier versions count toward the space used.</p></div>
          <Check checked={versioning} onChange={setVersioning} label="Keep versions of overwritten and deleted objects" description="A deleted or replaced file can be restored from its version history." />
        </div>
        <div className="grid gap-3 px-5 py-4 sm:grid-cols-[12rem_1fr]">
          <div><p className="font-medium">Uploads</p><p className="text-xs text-muted-foreground">Uploads that break these limits are refused.</p></div>
          <div className="space-y-4">
            <FormField label="Max object size" error={maxError} help="For example 10 MiB. Leave empty for no limit.">
              {(p) => <Input {...p} size="sm" className="w-40" placeholder="no limit" value={max} onChange={(e) => setMax(e.target.value)} />}
            </FormField>
            <FormField label="Allowed content types" help="One per line, such as image/png. Leave empty to allow any type.">
              {(p) => <Textarea {...p} size="sm" autoResize={false} className="block h-24 w-full max-w-md font-mono text-xs" placeholder={"image/png\nimage/jpeg"} value={types} onChange={(e) => setTypes(e.target.value)} />}
            </FormField>
          </div>
        </div>
      </div>
      {error !== undefined && <div className="border-t border-border px-5 py-3"><ErrorView error={error} /></div>}
      <footer className="flex justify-end border-t border-border bg-muted/30 px-5 py-3">
        <Button type="submit" tone="primary" disabled={busy}>{busy ? "Saving…" : "Save policy"}</Button>
      </footer>
    </form>
  );
}
