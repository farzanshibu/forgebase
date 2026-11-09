import { useState } from "react";
import { brandingAssetUrl, deleteBrandingAsset, getBranding, MAX_ASSET_BYTES, putBranding, putBrandingAsset, type AssetKind, type Branding } from "../../api";
import { Button, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { ActionError, Field } from "../access/shared";
import { useRun } from "../access/util";
import { Note } from "./shared";
import { useRealm } from "./util";

type Draft = Record<"display_name" | "primary_color" | "background_color" | "text_color" | "accent_color" | "signin_title" | "signin_subtitle" | "signin_footer" | "support_email", string>;
const KEYS = ["display_name", "primary_color", "background_color", "text_color", "accent_color", "signin_title", "signin_subtitle", "signin_footer", "support_email"] as const;
const COLORS = new Set(["primary_color", "background_color", "text_color", "accent_color"]);
const LABELS: Draft = { display_name: "Display name", primary_color: "Primary colour", background_color: "Background colour", text_color: "Text colour", accent_color: "Accent colour", signin_title: "Sign-in title", signin_subtitle: "Sign-in subtitle", signin_footer: "Sign-in footer", support_email: "Support email" };
const HEX = /^#[0-9a-fA-F]{3,8}$/;
const draftOf = (b: Branding): Draft => Object.fromEntries(KEYS.map((k) => [k, b[k] ?? ""])) as Draft;

/** White-label branding: colours, texts, logo and favicon upload, with a live preview of the hosted sign-in page. */
export default function BrandingPage() {
  const realm = useRealm();
  const l = useLoad((s) => getBranding(realm, s), `branding:${realm}`);
  return (
    <>
      <ErrorView error={l.error} />
      {l.loading && !l.error && <Loading what="Loading branding" />}
      {l.data && <BrandingEditor key={realm} realm={realm} b={l.data} reload={l.reload} />}
    </>
  );
}

function BrandingEditor({ realm, b, reload }: { realm: string; b: Branding; reload: () => void }) {
  const [d, setD] = useState<Draft>(() => draftOf(b));
  const [rev, setRev] = useState(0);
  const [saved, setSaved] = useState(false);
  const save = useRun(() => { setSaved(true); reload(); });
  const asset = useRun(() => { setRev((r) => r + 1); reload(); });
  const [fileError, setFileError] = useState("");
  const bad = KEYS.filter((k) => COLORS.has(k) && d[k] && !HEX.test(d[k]));
  const upload = (kind: AssetKind, f: File | undefined) => {
    if (!f) return;
    if (f.size > MAX_ASSET_BYTES) { setFileError(`An image is at most 256 KiB (this one is ${Math.ceil(f.size / 1024)} KiB)`); return; }
    setFileError("");
    void asset.run(() => putBrandingAsset(kind, f, realm));
  };
  return (
    <div className="grid gap-6 lg:grid-cols-2">
      <Panel title={`Branding of ${realm}`}>
        {(
          <form aria-label="Branding" className="space-y-3" onSubmit={(e) => {
            e.preventDefault(); setSaved(false);
            void save.run(() => putBranding(Object.fromEntries(KEYS.map((k) => [k, d[k] === "" ? null : d[k]])), realm));
          }}>
            {KEYS.map((k) => (
              <Field key={k} label={LABELS[k]}>
                <div className="flex items-center gap-2">
                  <input aria-label={LABELS[k]} className={`${inputCls} flex-1`} value={d[k]} placeholder={COLORS.has(k) ? "#rrggbb" : ""} onChange={(e) => setD({ ...d, [k]: e.target.value })} />
                  {COLORS.has(k) && HEX.test(d[k]) && <span aria-hidden className="h-6 w-6 rounded border border-zinc-600" style={{ background: d[k] }} />}
                </div>
              </Field>
            ))}
            {bad.length > 0 && <p role="alert" className="text-xs text-red-300">Colours must be hex like #1a2b3c ({bad.map((k) => LABELS[k]).join(", ")}).</p>}
            <div className="flex items-center gap-3">
              <Button type="submit" tone="primary" disabled={save.busy || bad.length > 0}>Save branding</Button>
              {saved && <span role="status" className="text-sm text-emerald-300">Saved</span>}
            </div>
            <ActionError error={save.error} />
          </form>
        )}
        {(
          <div className="mt-5 space-y-3 border-t border-zinc-800 pt-4">
            {(["logo", "favicon"] as const).map((kind) => (
              <div key={kind} className="flex flex-wrap items-center gap-3">
                <Field label={kind === "logo" ? "Logo" : "Favicon"}>
                  <input aria-label={kind === "logo" ? "Logo file" : "Favicon file"} type="file" accept="image/png,image/jpeg,image/svg+xml,image/x-icon,image/webp" onChange={(e) => upload(kind, e.target.files?.[0])} />
                </Field>
                {(kind === "logo" ? b.has_logo : b.has_favicon) && (
                  <>
                    <img alt={`Current ${kind}`} className="h-8 max-w-24 object-contain" src={`${brandingAssetUrl(kind, realm)}&v=${rev}`} />
                    <Button aria-label={`Remove ${kind}`} onClick={() => void asset.run(() => deleteBrandingAsset(kind, realm))}>Remove</Button>
                  </>
                )}
              </div>
            ))}
            <Note>PNG, JPEG, SVG, ICO or WebP, at most 256 KiB each.</Note>
            {fileError && <p role="alert" className="text-sm text-red-300">{fileError}</p>}
            <ActionError error={asset.error} />
          </div>
        )}
      </Panel>
      <Panel title="Live preview">
        {(
          <div aria-label="Sign-in preview" className="rounded-lg p-8 text-center" style={{ background: HEX.test(d.background_color) ? d.background_color : "#18181b", color: HEX.test(d.text_color) ? d.text_color : "#f4f4f5" }}>
            {b.has_logo && <img alt="Preview logo" className="mx-auto mb-3 h-10" src={`${brandingAssetUrl("logo", realm)}&v=${rev}`} />}
            <p className="text-sm opacity-70">{d.display_name || realm}</p>
            <h3 className="mt-1 text-xl font-semibold">{d.signin_title || "Sign in"}</h3>
            {d.signin_subtitle && <p className="mt-1 text-sm opacity-80">{d.signin_subtitle}</p>}
            <div className="mx-auto mt-4 max-w-xs space-y-2">
              <div className="rounded border border-current/30 px-3 py-2 text-left text-sm opacity-60">Email</div>
              <div className="rounded border border-current/30 px-3 py-2 text-left text-sm opacity-60">Password</div>
              <div className="rounded px-3 py-2 text-sm font-medium" style={{ background: HEX.test(d.primary_color) ? d.primary_color : "#f59e0b", color: "#0a0a0a" }}>Continue</div>
              <a className="block text-sm underline" style={{ color: HEX.test(d.accent_color) ? d.accent_color : "inherit" }}>Forgot password?</a>
            </div>
            {d.signin_footer && <p className="mt-4 text-xs opacity-70">{d.signin_footer}</p>}
            {d.support_email && <p className="mt-1 text-xs opacity-70">{d.support_email}</p>}
          </div>
        )}
      </Panel>
    </div>
  );
}
