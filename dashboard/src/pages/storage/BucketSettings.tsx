import { useState } from "react";
import { Input } from "panelui-web/input";
import { Textarea } from "panelui-web/textarea";
import { VISIBILITIES, bucketUsage, updateBucket } from "../../api";
import type { Bucket, Visibility } from "../../api";
import { Check, Sel } from "../../components/controls";
import { FormField } from "../../components/FormField";
import { Button, ErrorView, Panel } from "../../components/ui";
import { formatBytes } from "../../format";
import { useLoad } from "../../hooks";
import { notify } from "../../toaster";
import { parseBytes, parseTypes } from "./util";

export default function BucketSettings({ bucket, onSaved }: { bucket: Bucket; onSaved: () => void }) {
  const [visibility, setVisibility] = useState<Visibility>(bucket.visibility);
  const [versioning, setVersioning] = useState(bucket.versioning);
  const [max, setMax] = useState(bucket.max_object_bytes ? String(bucket.max_object_bytes) : "");
  const [types, setTypes] = useState((bucket.allowed_content_types ?? []).join("\n"));
  const [error, setError] = useState<unknown>();
  const [maxError, setMaxError] = useState<string>();
  const usage = useLoad((s) => bucketUsage(bucket.name, s), `bucket-usage:${bucket.name}`);
  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    setError(undefined);
    setMaxError(undefined);
    let bytes: number | null;
    try { bytes = parseBytes(max); } catch (err) { setMaxError((err as Error).message); return; }
    try {
      await updateBucket(bucket.name, { visibility, versioning, max_object_bytes: bytes, allowed_content_types: parseTypes(types) });
      notify.saved("Bucket policy");
      onSaved();
    } catch (err) { setError(err); }
  };
  return (
    <Panel title="Bucket policy">
      {usage.data && (
        <p aria-label="Bucket space" className="mb-3 text-sm text-muted-foreground">
          Uses {formatBytes(usage.data.logical_bytes)} in {usage.data.objects} objects; {formatBytes(usage.data.version_bytes)} across {usage.data.versions} versions.
        </p>
      )}
      <form onSubmit={(e) => void save(e)} className="max-w-xl space-y-4 text-sm" noValidate>
        <FormField label="Visibility" help="Private: only the owner and people on the access list. Authenticated: any signed-in caller can read. Public: anyone can read.">
          {(p) => <Sel {...p} aria-label="Visibility" value={visibility} onChange={(v) => setVisibility(v as Visibility)} options={VISIBILITIES.map((v) => ({ value: v, label: v }))} />}
        </FormField>
        <Check checked={versioning} onChange={setVersioning} label="Keep versions of overwritten and deleted objects" description="A deleted or replaced file can be restored from its version history." />
        <FormField label="Max object size" error={maxError} help="For example 10 MiB. Leave empty for no limit.">
          {(p) => <Input {...p} size="sm" className="w-40" placeholder="no limit" value={max} onChange={(e) => setMax(e.target.value)} />}
        </FormField>
        <FormField label="Allowed content types" help="One per line, such as image/png. Leave empty to allow any type.">
          {(p) => <Textarea {...p} size="sm" autoResize={false} className="block h-24 w-full font-mono text-xs" value={types} onChange={(e) => setTypes(e.target.value)} />}
        </FormField>
        <Button type="submit" tone="primary">Save policy</Button>
        <ErrorView error={error} />
      </form>
    </Panel>
  );
}
