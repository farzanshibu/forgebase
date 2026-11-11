/**
 * Features and settings in System: the feature map and every enabled feature's settings on one page, one Save.
 * (First-run setup keeps its three steps in FeaturesFlow.) Edits stay in a draft; the sticky bar sends one PATCH with
 * the changed keys only, toasts "Settings saved" and says when a restart is needed.
 */
import { useState } from "react";
import { Alert } from "panelui-web/alert";
import type { FeaturesResponse, ServerFeature } from "../../api";
import { ErrorPanel } from "../../components/ErrorPanel";
import { Button } from "../../components/ui";
import { notify } from "../../toaster";
import FeatureCard from "./FeatureCard";
import SettingsSections from "./SettingsSections";
import { enabledKey, enabledSetting } from "./settingUtil";
import { useSettingsDraft } from "./useSettingsDraft";

export default function FeaturesForm({ snapshot, apply }: {
  snapshot: FeaturesResponse;
  /** Take the features a save answered with. */
  apply: (features: ServerFeature[], restartPending?: boolean) => void;
}) {
  const features = snapshot.features;
  const d = useSettingsDraft(features);
  const [restart, setRestart] = useState(false);

  async function save() {
    const n = d.changed.length;
    const r = await d.save();
    if ("fields" in r) return;
    apply(r.features, r.restart_required ? true : undefined);
    setRestart(r.restart_required);
    notify.saved(n === 1 ? "Setting" : `${n} settings`, r.restart_required ? "Restart the server for it to take effect." : "It is already in effect.");
  }

  const sections = features.filter((f) => f.always_on || d.isOn(f));
  const visible = new Set(sections.flatMap((f) => f.settings.map((s) => s.key)));
  const elsewhere = Object.entries(d.serverErr).filter(([k]) => !visible.has(k) && !k.endsWith(".enabled"));
  const errorOf = (k: string) => d.clientErr[k] ?? d.serverErr[k];
  const hasClientErr = Object.keys(d.clientErr).length > 0;

  return (
    <div className="space-y-6 pb-20">
      {(snapshot.restart_pending || restart) && (
        <Alert tone="warning" variant="plain">
          <Alert.Icon />
          <Alert.Title>Restart the server to finish</Alert.Title>
          <Alert.Description>
            Changes in <code>{snapshot.config_file}</code> take effect when the server restarts: stop and run <code>forgebase serve</code> again, or restart the service with your service manager (for example <code>systemctl restart forgebase</code>).
          </Alert.Description>
        </Alert>
      )}

      <section aria-label="Feature map" className="space-y-3">
        <div>
          <h2 className="text-lg font-semibold tracking-tight">Features</h2>
          <p className="text-sm text-muted-foreground">Choose what this server runs. A feature that is off answers “disabled” and starts nothing.</p>
        </div>
        <ul className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
          {features.map((f) => {
            const es = enabledSetting(f);
            return (
              <li key={f.id}>
                <FeatureCard feature={f} on={f.always_on ? true : d.isOn(f)} error={d.serverErr[enabledKey(f)]} onToggle={(on) => es && d.edit(es, on)} />
              </li>
            );
          })}
        </ul>
      </section>

      <section aria-label="Settings" className="space-y-3">
        <div>
          <h2 className="text-lg font-semibold tracking-tight">Settings</h2>
          <p className="text-sm text-muted-foreground">
            Settings of the features that are on. Saving writes only what you changed to <code>{snapshot.config_file}</code>.
          </p>
        </div>
        <SettingsSections features={sections} valueOf={d.valueOf} errorOf={errorOf} onEdit={d.edit} />
        {elsewhere.length > 0 && (
          <Alert tone="destructive" variant="plain">
            <Alert.Icon />
            <Alert.Title>The server rejected these settings</Alert.Title>
            <Alert.Description>
              <ul className="list-disc pl-5">
                {elsewhere.map(([k, m]) => (
                  <li key={k}>
                    <code>{k}</code>: {m}
                  </li>
                ))}
              </ul>
            </Alert.Description>
          </Alert>
        )}
        {d.saveErr !== undefined && Object.keys(d.serverErr).length === 0 && <ErrorPanel error={d.saveErr} />}
      </section>

      <div role="region" aria-label="Save changes" className="sticky bottom-0 -mx-1 flex items-center justify-between gap-3 border-t border-border bg-background/95 px-1 py-3 backdrop-blur">
        <span className="text-sm text-muted-foreground" aria-live="polite">
          {d.changed.length === 0 ? "No changes" : `${d.changed.length} unsaved ${d.changed.length === 1 ? "change" : "changes"}`}
        </span>
        <div className="flex gap-2">
          <Button disabled={d.changed.length === 0 || d.busy} onClick={d.discard}>Discard</Button>
          <Button tone="primary" disabled={d.changed.length === 0 || d.busy || hasClientErr} onClick={() => void save()}>
            {d.busy ? "Saving…" : "Save changes"}
          </Button>
        </div>
      </div>
    </div>
  );
}
