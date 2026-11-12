import { useState } from "react";
import { Link01Icon } from "@hugeicons/core-free-icons";
import { Input } from "panelui-web/input";
import { Textarea } from "panelui-web/textarea";
import { signUrl } from "../../api";
import type { Bucket, SignedUrl } from "../../api";
import { Sel } from "../../components/controls";
import { FormField } from "../../components/FormField";
import { HelpTip } from "../../components/HelpTip";
import { Button, ErrorView } from "../../components/ui";
import { notify } from "../../toaster";
import { when } from "../functions/util";
import { Icon } from "../database/parts";
import { EXPIRY_PRESETS } from "./util";

/**
 * Signed URL generator for any key (also in an object's details with `initialKey`). `compact` stacks the fields for
 * the narrow details column.
 */
export default function SignedUrls({ bucket, initialKey = "", initialVersion = "", compact = false }: { bucket: Bucket; initialKey?: string; initialVersion?: string; compact?: boolean }) {
  const [key, setKey] = useState(initialKey);
  const [method, setMethod] = useState<"GET" | "PUT">("GET");
  const [expiry, setExpiry] = useState(3600);
  const [downloadAs, setDownloadAs] = useState("");
  const [version, setVersion] = useState(initialVersion);
  const [out, setOut] = useState<SignedUrl>();
  const [error, setError] = useState<unknown>();
  const [busy, setBusy] = useState(false);
  const make = async (e: React.FormEvent) => {
    e.preventDefault();
    setError(undefined); setOut(undefined); setBusy(true);
    try {
      setOut(await signUrl(bucket.name, { key, method, expires_in: expiry, ...(downloadAs && method === "GET" ? { download_as: downloadAs } : {}), ...(version && method === "GET" ? { version_id: version } : {}) }));
    } catch (err) { setError(err); } finally { setBusy(false); }
  };
  const grid = compact ? "grid grid-cols-2 gap-3" : "grid gap-4 sm:grid-cols-2 lg:grid-cols-3";
  return (
    <section className={`rounded-xl border border-border bg-card ${compact ? "" : "max-w-4xl"}`}>
      <header className="border-b border-border px-4 py-2.5">
        <h3 className="flex items-center gap-2 text-sm font-medium"><Icon icon={Link01Icon} size={15} className="text-muted-foreground" />Signed URL</h3>
      </header>
      <div className="p-4">
        <p className="mb-4 text-xs text-muted-foreground"><HelpTip term="signed_url">A signed URL</HelpTip> lets someone download or upload one file without a key until it expires. It is checked when issued, not when used: rotate the signing key to revoke every URL.</p>
        <form onSubmit={(e) => void make(e)} className="space-y-4 text-sm">
          <div className={grid}>
            <FormField label="Key" required className={compact ? "col-span-2" : "sm:col-span-2 lg:col-span-1"}>{(p) => <Input {...p} size="sm" className="font-mono" placeholder="folder/file.png" value={key} onChange={(e) => setKey(e.target.value)} />}</FormField>
            <FormField label="Method">{(p) => <Sel {...p} aria-label="Method" className="w-full" value={method} onChange={(v) => setMethod(v as "GET" | "PUT")} options={[{ value: "GET", label: "GET (download)" }, { value: "PUT", label: "PUT (upload)" }]} />}</FormField>
            <FormField label="Expires in">{(p) => <Sel {...p} aria-label="Expires in" className="w-full" value={expiry} onChange={setExpiry} options={EXPIRY_PRESETS.map((x) => ({ value: x.seconds, label: x.label }))} />}</FormField>
            {method === "GET" && <FormField label="Download as" help="Optional file name.">{(p) => <Input {...p} size="sm" placeholder="file name" value={downloadAs} onChange={(e) => setDownloadAs(e.target.value)} />}</FormField>}
            {method === "GET" && <FormField label="Version" help="Empty: the current one.">{(p) => <Input {...p} size="sm" className="font-mono" placeholder="current" value={version} onChange={(e) => setVersion(e.target.value)} />}</FormField>}
          </div>
          <div className="flex justify-end">
            <Button type="submit" tone="primary" disabled={!key || busy}>Generate URL</Button>
          </div>
        </form>
        <ErrorView error={error} />
        {out && (
          <div className="mt-4 space-y-2 rounded-lg border border-border bg-muted/30 p-3 text-sm">
            <Textarea size="sm" autoResize={false} readOnly aria-label="Signed URL" className="block h-20 w-full font-mono text-xs" value={out.url} onFocus={(e) => e.currentTarget.select()} />
            <div className="flex flex-wrap items-center justify-between gap-2">
              <p className="text-xs text-muted-foreground">{out.method} · expires {when(out.expires_at)}</p>
              <Button
                onClick={() => {
                  void navigator.clipboard?.writeText(out.url);
                  notify.copied("Signed URL");
                }}
              >
                Copy URL
              </Button>
            </div>
          </div>
        )}
      </div>
    </section>
  );
}
