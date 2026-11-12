import { useState } from "react";
import { brandingAssetUrl, deleteBrandingAsset, getBranding, MAX_ASSET_BYTES, putBranding, putBrandingAsset, type AssetKind, type Branding } from "../../api";
import { Button } from "../../components/ui";
import { SkeletonCards } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import { ActionError } from "../access/shared";
import { useRun } from "../access/util";
import { FormSection } from "../billing/shared";
import { Saved } from "./shared";
import { useRealm } from "./util";
import { Input } from "panelui-web/input";

type Key = "display_name" | "primary_color" | "background_color" | "text_color" | "accent_color" | "signin_title" | "signin_subtitle" | "signin_footer" | "support_email";
type Draft = Record<Key, string>;
const KEYS: readonly Key[] = ["display_name", "primary_color", "background_color", "text_color", "accent_color", "signin_title", "signin_subtitle", "signin_footer", "support_email"];
const COLOR_KEYS: readonly Key[] = ["primary_color", "accent_color", "background_color", "text_color"];
const LABELS: Draft = { display_name: "Display name", primary_color: "Primary colour", background_color: "Background colour", text_color: "Text colour", accent_color: "Accent colour", signin_title: "Sign-in title", signin_subtitle: "Sign-in subtitle", signin_footer: "Sign-in footer", support_email: "Support email" };
const HELP: Partial<Draft> = {
  display_name: "Your product or company name, above the sign-in form.",
  support_email: "Shown under the form so users can ask for help.",
  primary_color: "Buttons.", accent_color: "Links.", background_color: "The page behind the form.", text_color: "Headings and labels.",
  signin_footer: "A line under the form, such as a copyright.",
};
const PLACEHOLDER: Partial<Draft> = { display_name: "Acme", support_email: "help@example.com", signin_title: "Sign in", signin_subtitle: "Welcome back", signin_footer: "© Acme Inc." };
const HEX = /^#[0-9a-fA-F]{3,8}$/;
const draftOf = (b: Branding): Draft => Object.fromEntries(KEYS.map((k) => [k, b[k] ?? ""])) as Draft;
/** A colour input only takes #rrggbb: widen #rgb, drop alpha, fall back to black. */
const six = (v: string) => (/^#[0-9a-f]{6}/i.test(v) ? v.slice(0, 7) : /^#[0-9a-f]{3}$/i.test(v) ? `#${[...v.slice(1)].map((c) => c + c).join("")}` : "#000000");

/** White-label branding: colours, texts, logo and favicon upload, with a live preview of the hosted sign-in page. */
export default function BrandingPage() {
  const realm = useRealm();
  const l = useLoad((s) => getBranding(realm, s), `branding:${realm}`);
  return (
    <>
      <ActionError error={l.error} />
      {l.loading && !l.error && <SkeletonCards count={2} label="Loading branding" />}
      {l.data && <BrandingEditor key={realm} realm={realm} b={l.data} reload={l.reload} />}
    </>
  );
}

function TextInput({ k, d, set }: { k: Key; d: Draft; set: (d: Draft) => void }) {
  return (
    <div className="flex min-w-0 flex-col gap-1.5">
      <label className="flex flex-col gap-1.5 text-sm font-medium">
        {LABELS[k]}
        <Input size="sm" aria-label={LABELS[k]} className="font-normal" value={d[k]} placeholder={PLACEHOLDER[k]} onChange={(e) => set({ ...d, [k]: e.target.value })} />
      </label>
      {HELP[k] && <p className="text-xs text-muted-foreground">{HELP[k]}</p>}
    </div>
  );
}

function ColorInput({ k, d, set }: { k: Key; d: Draft; set: (d: Draft) => void }) {
  const v = d[k];
  const bad = v !== "" && !HEX.test(v);
  return (
    <div className="flex min-w-0 flex-col gap-1.5">
      <span className="text-sm font-medium" id={`lbl-${k}`}>{LABELS[k]}</span>
      <span className="flex items-center gap-2">
        <span className={`relative size-8 shrink-0 overflow-hidden rounded-md border border-border ${HEX.test(v) ? "" : "border-dashed bg-muted/40"}`.trim()} style={HEX.test(v) ? { background: v } : undefined} title={HEX.test(v) ? v : "Default"}>
          <input type="color" aria-label={`Pick ${LABELS[k].toLowerCase()}`} value={six(v)} onChange={(e) => set({ ...d, [k]: e.target.value })} className="absolute inset-0 size-full cursor-pointer opacity-0" />
        </span>
        <Input size="sm" aria-label={LABELS[k]} aria-invalid={bad || undefined} className="font-mono" value={v} placeholder="#rrggbb" onChange={(e) => set({ ...d, [k]: e.target.value })} />
      </span>
      {HELP[k] && <p className="text-xs text-muted-foreground">{HELP[k]}</p>}
    </div>
  );
}

function BrandingEditor({ realm, b, reload }: { realm: string; b: Branding; reload: () => void }) {
  const [d, setD] = useState<Draft>(() => draftOf(b));
  const [rev, setRev] = useState(0);
  const [saved, setSaved] = useState(false);
  const save = useRun(() => { setSaved(true); reload(); });
  const asset = useRun(() => { setRev((r) => r + 1); reload(); });
  const [fileError, setFileError] = useState("");
  const bad = COLOR_KEYS.filter((k) => d[k] && !HEX.test(d[k]));
  const dirty = KEYS.some((k) => d[k] !== (b[k] ?? ""));
  const upload = (kind: AssetKind, f: File | undefined) => {
    if (!f) return;
    if (f.size > MAX_ASSET_BYTES) { setFileError(`An image is at most 256 KiB (this one is ${Math.ceil(f.size / 1024)} KiB)`); return; }
    setFileError("");
    void asset.run(() => putBrandingAsset(kind, f, realm));
  };
  const color = (k: Key, fallback: string) => (HEX.test(d[k]) ? d[k] : fallback);
  const edit = (n: Draft) => { setD(n); setSaved(false); };
  return (
    <div className="grid items-start gap-6 xl:grid-cols-[minmax(0,1fr)_minmax(0,26rem)]">
      <div className="space-y-6">
        <form aria-label="Branding" className="space-y-5 rounded-xl border border-border bg-card p-5" onSubmit={(e) => {
          e.preventDefault(); setSaved(false);
          void save.run(() => putBranding(Object.fromEntries(KEYS.map((k) => [k, d[k] === "" ? null : d[k]])), realm));
        }}>
          <FormSection title="Identity" description={`How realm ${realm} names itself on the hosted sign-in pages and in emails.`}>
            <div className="grid gap-3 sm:grid-cols-2">
              <TextInput k="display_name" d={d} set={edit} />
              <TextInput k="support_email" d={d} set={edit} />
            </div>
          </FormSection>
          <FormSection title="Colours" description="Hex values like #1a2b3c. Blank uses the default.">
            <div className="grid gap-3 sm:grid-cols-2">
              {COLOR_KEYS.map((k) => <ColorInput key={k} k={k} d={d} set={edit} />)}
            </div>
            {bad.length > 0 && <p role="alert" className="text-xs font-medium text-destructive">Colours must be hex like #1a2b3c ({bad.map((k) => LABELS[k]).join(", ")}).</p>}
          </FormSection>
          <FormSection title="Sign-in page">
            <div className="grid gap-3 sm:grid-cols-2">
              <TextInput k="signin_title" d={d} set={edit} />
              <TextInput k="signin_subtitle" d={d} set={edit} />
            </div>
            <TextInput k="signin_footer" d={d} set={edit} />
          </FormSection>
          <ActionError error={save.error} />
          <div className="flex items-center gap-3 border-t border-border pt-4">
            <Button type="submit" tone="primary" disabled={save.busy || bad.length > 0}>{save.busy ? "Saving…" : "Save branding"}</Button>
            {dirty && !save.busy && <Button type="button" onClick={() => edit(draftOf(b))}>Discard changes</Button>}
            <Saved show={saved} />
          </div>
        </form>
        <section aria-labelledby="brand-images" className="space-y-4 rounded-xl border border-border bg-card p-5">
          <div>
            <h3 id="brand-images" className="text-sm font-semibold">Images</h3>
            <p className="mt-0.5 text-xs text-muted-foreground">PNG, JPEG, SVG, ICO or WebP, at most 256 KiB each. Uploads apply at once.</p>
          </div>
          {(["logo", "favicon"] as const).map((kind) => {
            const has = kind === "logo" ? b.has_logo : b.has_favicon;
            const id = `brand-${kind}`;
            return (
              <div key={kind} className="flex flex-wrap items-center gap-4 rounded-lg border border-border p-3">
                <span className="flex size-14 shrink-0 items-center justify-center overflow-hidden rounded-md border border-dashed border-border bg-muted/40">
                  {has ? <img alt={`Current ${kind}`} className="max-h-12 max-w-12 object-contain" src={`${brandingAssetUrl(kind, realm)}&v=${rev}`} /> : <span className="text-xs text-muted-foreground">None</span>}
                </span>
                <span className="min-w-0 flex-1">
                  <span className="block text-sm font-medium">{kind === "logo" ? "Logo" : "Favicon"}</span>
                  <span className="block text-xs text-muted-foreground">{kind === "logo" ? "Shown above the sign-in form." : "The icon in the browser tab."}</span>
                </span>
                <span className="flex gap-2">
                  <input id={id} aria-label={kind === "logo" ? "Logo file" : "Favicon file"} type="file" className="peer sr-only" accept="image/png,image/jpeg,image/svg+xml,image/x-icon,image/webp" onChange={(e) => { upload(kind, e.target.files?.[0]); e.target.value = ""; }} />
                  <label htmlFor={id} className="inline-flex h-8 cursor-pointer items-center rounded-md border border-border bg-background px-3 text-sm font-medium hover:bg-muted peer-focus-visible:outline-2 peer-focus-visible:outline-ring">
                    {has ? "Replace" : "Upload"}
                  </label>
                  {has && <Button aria-label={`Remove ${kind}`} onClick={() => void asset.run(() => deleteBrandingAsset(kind, realm))}>Remove</Button>}
                </span>
              </div>
            );
          })}
          {fileError && <p role="alert" className="text-sm font-medium text-destructive">{fileError}</p>}
          <ActionError error={asset.error} />
        </section>
      </div>
      <section aria-labelledby="brand-preview" className="space-y-2 xl:sticky xl:top-4">
        <div className="flex items-center justify-between">
          <h3 id="brand-preview" className="text-sm font-semibold">Live preview</h3>
          <span className="text-xs text-muted-foreground">Updates as you type</span>
        </div>
        <div className="overflow-hidden rounded-xl border border-border shadow-sm">
          <div className="flex items-center gap-2 border-b border-border bg-muted/60 px-3 py-2" aria-hidden="true">
            <span className="flex gap-1"><span className="size-2.5 rounded-full bg-foreground/15" /><span className="size-2.5 rounded-full bg-foreground/15" /><span className="size-2.5 rounded-full bg-foreground/15" /></span>
            <span className="ml-2 flex min-w-0 items-center gap-1.5 rounded-md bg-background px-2 py-0.5 text-xs text-muted-foreground">
              {b.has_favicon && <img alt="" className="size-3.5" src={`${brandingAssetUrl("favicon", realm)}&v=${rev}`} />}
              <span className="truncate">{d.display_name || realm} · Sign in</span>
            </span>
          </div>
          <div aria-label="Sign-in preview" className="px-6 py-10 text-center" style={{ background: color("background_color", "#18181b"), color: color("text_color", "#f4f4f5") }}>
            {b.has_logo && <img alt="Preview logo" className="mx-auto mb-3 h-10" src={`${brandingAssetUrl("logo", realm)}&v=${rev}`} />}
            <p className="text-sm opacity-70">{d.display_name || realm}</p>
            <h4 className="mt-1 text-xl font-semibold">{d.signin_title || "Sign in"}</h4>
            {d.signin_subtitle && <p className="mt-1 text-sm opacity-80">{d.signin_subtitle}</p>}
            <div className="mx-auto mt-5 max-w-xs space-y-2">
              <div className="rounded-md border border-current/25 px-3 py-2 text-left text-sm opacity-60">Email</div>
              <div className="rounded-md border border-current/25 px-3 py-2 text-left text-sm opacity-60">Password</div>
              <div className="rounded-md px-3 py-2 text-sm font-medium" style={{ background: color("primary_color", "#f59e0b"), color: "#0a0a0a" }}>Continue</div>
              <span className="block pt-1 text-sm underline" style={{ color: color("accent_color", "inherit") }}>Forgot password?</span>
            </div>
            {d.signin_footer && <p className="mt-6 text-xs opacity-70">{d.signin_footer}</p>}
            {d.support_email && <p className="mt-1 text-xs opacity-70">{d.support_email}</p>}
          </div>
        </div>
        <p className="text-xs text-muted-foreground">The button text colour is fixed dark; pick a light enough primary colour for contrast.</p>
      </section>
    </div>
  );
}
