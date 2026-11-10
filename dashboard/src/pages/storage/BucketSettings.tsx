import { useState } from "react";
import { VISIBILITIES, bucketUsage, updateBucket } from "../../api";
import type { Bucket, Visibility } from "../../api";
import { Button, ErrorView, Panel } from "../../components/ui";
import { formatBytes } from "../../format";
import { useLoad } from "../../hooks";
import { parseBytes, parseTypes } from "./util";
import { Input } from "panelui-web/input";
import { Textarea } from "panelui-web/textarea";
import { Sel } from "../../components/controls";

export default function BucketSettings({ bucket, onSaved }: { bucket: Bucket; onSaved: () => void }) {
  const [visibility, setVisibility] = useState<Visibility>(bucket.visibility);
  const [versioning, setVersioning] = useState(bucket.versioning);
  const [max, setMax] = useState(bucket.max_object_bytes ? String(bucket.max_object_bytes) : "");
  const [types, setTypes] = useState((bucket.allowed_content_types ?? []).join("\n"));
  const [error, setError] = useState<unknown>();
  const [saved, setSaved] = useState(false);
  const usage = useLoad((s) => bucketUsage(bucket.name, s), `bucket-usage:${bucket.name}`);
  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    setError(undefined); setSaved(false);
    try {
      await updateBucket(bucket.name, { visibility, versioning, max_object_bytes: parseBytes(max), allowed_content_types: parseTypes(types) });
      setSaved(true);
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
      <form onSubmit={(e) => void save(e)} className="max-w-xl space-y-3 text-sm">
        <label className="block">Visibility
          <Sel aria-label="Visibility" className="ml-2" value={visibility} onChange={(v) => setVisibility(v as Visibility)} options={VISIBILITIES.map((v) => ({ value: v, label: v }))} />
        </label>
        <p className="text-xs text-muted-foreground">private: only the owner and ACL grantees; authenticated: any signed-in caller can read; public: anyone can read.</p>
        <label className="flex items-center gap-2"><input type="checkbox" checked={versioning} onChange={(e) => setVersioning(e.target.checked)} />Keep versions of overwritten and deleted objects</label>
        <label className="block">Max object size<Input size="sm" className="ml-2 w-32" placeholder="no limit" value={max} onChange={(e) => setMax(e.target.value)} /></label>
        <label className="block">Allowed content types (one per line, blank = any)
          <Textarea size="sm" autoResize={false} className="mt-1 block h-24 w-full font-mono text-xs" value={types} onChange={(e) => setTypes(e.target.value)} />
        </label>
        <div className="flex items-center gap-3">
          <Button type="submit" tone="primary">Save policy</Button>
          {saved && <span role="status" className="text-success">Saved.</span>}
        </div>
        <ErrorView error={error} />
      </form>
    </Panel>
  );
}
