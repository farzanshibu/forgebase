import { useState } from "react";
import { signUrl } from "../../api";
import type { Bucket, SignedUrl } from "../../api";
import { Button, ErrorView, Panel } from "../../components/ui";
import { when } from "../functions/util";
import { EXPIRY_PRESETS } from "./util";
import { Input } from "panelui-web/input";
import { notify } from "../../toaster";
import { FormField } from "../../components/FormField";
import { HelpTip } from "../../components/HelpTip";
import { Textarea } from "panelui-web/textarea";
import { Sel } from "../../components/controls";

/** Signed URL generator for any key (also reachable from an object's panel with `initialKey`). */
export default function SignedUrls({ bucket, initialKey = "", initialVersion = "" }: { bucket: Bucket; initialKey?: string; initialVersion?: string }) {
  const [key, setKey] = useState(initialKey);
  const [method, setMethod] = useState<"GET" | "PUT">("GET");
  const [expiry, setExpiry] = useState(3600);
  const [downloadAs, setDownloadAs] = useState("");
  const [version, setVersion] = useState(initialVersion);
  const [out, setOut] = useState<SignedUrl>();
  const [error, setError] = useState<unknown>();
  const make = async (e: React.FormEvent) => {
    e.preventDefault();
    setError(undefined); setOut(undefined);
    try {
      setOut(await signUrl(bucket.name, { key, method, expires_in: expiry, ...(downloadAs && method === "GET" ? { download_as: downloadAs } : {}), ...(version && method === "GET" ? { version_id: version } : {}) }));
    } catch (err) { setError(err); }
  };
  return (
    <Panel title="Signed URL">
      <p className="mb-3 text-sm text-muted-foreground"><HelpTip term="signed_url">A signed URL</HelpTip> lets someone download or upload one file without a key until it expires. It is checked when issued, not when used: rotate the signing key to revoke every URL.</p>
      <form onSubmit={(e) => void make(e)} className="flex flex-wrap items-end gap-3 text-sm">
        <FormField label="Key" required>{(p) => <Input {...p} size="sm" className="w-56" value={key} onChange={(e) => setKey(e.target.value)} />}</FormField>
        <FormField label="Method">{(p) => <Sel {...p} aria-label="Method" value={method} onChange={(v) => setMethod(v as "GET" | "PUT")} options={[{ value: "GET", label: "GET (download)" }, { value: "PUT", label: "PUT (upload)" }]} />}</FormField>
        <FormField label="Expires in">{(p) => <Sel {...p} aria-label="Expires in" value={expiry} onChange={setExpiry} options={EXPIRY_PRESETS.map((x) => ({ value: x.seconds, label: x.label }))} />}</FormField>
        {method === "GET" && <FormField label="Download as">{(p) => <Input {...p} size="sm" className="w-36" placeholder="file name" value={downloadAs} onChange={(e) => setDownloadAs(e.target.value)} />}</FormField>}
        {method === "GET" && <FormField label="Version">{(p) => <Input {...p} size="sm" className="w-32" placeholder="current" value={version} onChange={(e) => setVersion(e.target.value)} />}</FormField>}
        <Button type="submit" tone="primary" disabled={!key}>Generate URL</Button>
      </form>
      <ErrorView error={error} />
      {out && (
        <div className="mt-3 space-y-1 text-sm">
          <Textarea size="sm" autoResize={false} readOnly aria-label="Signed URL" className="block h-20 w-full font-mono text-xs" value={out.url} />
          <p className="text-muted-foreground">{out.method} · expires {when(out.expires_at)}</p>
          <Button
            onClick={() => {
              void navigator.clipboard?.writeText(out.url);
              notify.copied("Signed URL");
            }}
          >
            Copy URL
          </Button>
        </div>
      )}
    </Panel>
  );
}
