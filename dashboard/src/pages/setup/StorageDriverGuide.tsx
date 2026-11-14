import { CopyIcon } from "../../components/Button";
import type { FeatureSetting, ServerFeature, SettingValue } from "../../api";
import { Badge } from "panelui-web/badge";
import { DRIVER_ENV } from "./storageDrivers";

const sourceOf = (f: ServerFeature, key: string | undefined): FeatureSetting["source"] | undefined => (key ? f.settings.find((s) => s.key === key)?.source : undefined);

/**
 * Credentials for the chosen storage driver: which environment variables to set, and whether the server sees them. The form never takes a
 * secret: the keys, tokens and account keys are environment-only, so they are neither stored in the database nor written to the config file.
 */
export default function StorageDriverGuide({ feature, driver }: { feature: ServerFeature; driver: SettingValue }) {
  const d = typeof driver === "string" ? driver : "local";
  const info = DRIVER_ENV[d];
  if (!info) {
    return (
      <section aria-label="Storage credentials" className="rounded-lg border border-border bg-muted/40 p-3 text-sm text-muted-foreground">
        The <code className="font-mono">local</code> driver keeps files under the data directory and needs no credentials. To use a cloud store, choose <code className="font-mono">s3</code>, <code className="font-mono">gcs</code> or <code className="font-mono">azure</code>: its settings and the environment variables it needs appear here.
      </section>
    );
  }
  return (
    <section aria-label="Storage credentials" className="space-y-2 rounded-lg border border-border bg-muted/40 p-3 text-sm">
      <h4 className="font-semibold">Credentials for {info.label}</h4>
      <p className="text-xs text-muted-foreground">{info.intro} <strong>Credentials are environment-only</strong>: set them where the server starts, then restart. They are never typed into this form, stored in the database or written to the config file.</p>
      <ul aria-label={`${info.label} environment variables`} className="space-y-1.5">
        {info.vars.map((v) => {
          const src = sourceOf(feature, v.key);
          return (
            <li key={v.name} className="flex flex-wrap items-center gap-x-2 gap-y-1">
              <code className="rounded border border-border bg-background px-1.5 py-0.5 font-mono text-xs">{v.name}</code>
              <CopyIcon text={v.name} label={`Copy ${v.name}`} />
              {v.key && <Badge variant={src === "env" ? "success" : "outline"} size="sm">{src === "env" ? "set" : "not set"}</Badge>}
              <span className="text-xs text-muted-foreground">{v.what}</span>
            </li>
          );
        })}
      </ul>
      {info.fallback && <p className="text-xs text-muted-foreground">{info.fallback}</p>}
    </section>
  );
}
