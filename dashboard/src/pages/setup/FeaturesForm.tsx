/**
 * Features and settings in System: the feature map and every enabled feature's settings on one page, one Save.
 * (First-run setup keeps its three steps in FeaturesFlow.) Edits stay in a draft; the sticky bar sends one PATCH with
 * the changed keys only, toasts "Settings saved" and says when a restart is needed.
 */
import { useState } from "react";
import { Alert } from "panelui-web/alert";
import { HugeiconsIcon } from "@hugeicons/react";
import type { FeaturesResponse, ServerFeature } from "../../api";
import { ErrorPanel } from "../../components/ErrorPanel";
import { SaveBar } from "../../components/SaveBar";
import { Button } from "../../components/ui";
import { notify } from "../../toaster";
import FeatureCard from "./FeatureCard";
import SettingsSections from "./SettingsSections";
import { featureIcon } from "./featureIcons";
import { enabledKey, enabledSetting, groupSettings, settingsAnchor } from "./settingUtil";
import { useSettingsDraft } from "./useSettingsDraft";

export default function FeaturesForm({ snapshot, apply }: {
  snapshot: FeaturesResponse;
  /** Take the features a save answered with. */
  apply: (features: ServerFeature[], restartPending?: boolean) => void;
}) {
  const features = snapshot.features;
  const d = useSettingsDraft();
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
  const onCount = features.filter((f) => f.always_on || d.isOn(f)).length;
  const withSettings = sections.filter((f) => groupSettings(f).length > 0);
  const jump = (id: string) => document.getElementById(settingsAnchor(id))?.scrollIntoView({ behavior: "smooth", block: "start" });

  return (
    <div className="space-y-6">
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
          <div className="flex flex-wrap items-baseline justify-between gap-2">
            <h2 className="text-lg font-semibold tracking-tight">Features</h2>
            <span className="text-sm text-muted-foreground tabular-nums">
              {onCount} of {features.length} on
            </span>
          </div>
          <p className="text-sm text-muted-foreground">Choose what this server runs. A feature that is off answers “disabled” and starts nothing.</p>
          <p className="text-sm text-muted-foreground" data-testid="database-engines-note">The database engine is chosen when the project is created: PostgreSQL (the default) or SQLite (one file). <code>forgebase db convert --to postgres</code> moves an SQLite project.</p>
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
        <div className="grid items-start gap-6 xl:grid-cols-[12rem_minmax(0,1fr)]">
          {/* A long page: a sticky list of the feature cards below, so a setting is two clicks away, not a long scroll. */}
          <nav aria-label="Jump to settings" className="sticky top-0 hidden xl:block">
            <p className="mb-2 px-2 text-xs font-medium text-muted-foreground">Jump to</p>
            <ul className="space-y-0.5">
              {withSettings.map((f) => (
                <li key={f.id}>
                  <button
                    type="button"
                    onClick={() => jump(f.id)}
                    className="flex w-full cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 text-start text-sm text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
                  >
                    <HugeiconsIcon icon={featureIcon(f.id)} size={15} strokeWidth={1.8} aria-hidden="true" />
                    <span className="truncate">{f.name}</span>
                  </button>
                </li>
              ))}
            </ul>
          </nav>
          <div className="min-w-0 space-y-4">
            <SettingsSections features={sections} valueOf={d.valueOf} errorOf={errorOf} onEdit={d.edit} />
          </div>
        </div>
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

      <SaveBar label="Save changes" className="max-w-5xl">
        <span className="text-sm text-muted-foreground" aria-live="polite">
          {d.changed.length === 0 ? "No changes" : `${d.changed.length} unsaved ${d.changed.length === 1 ? "change" : "changes"}`}
        </span>
        <div className="flex gap-2">
          <Button disabled={d.changed.length === 0 || d.busy} onClick={d.discard}>Discard</Button>
          <Button tone="primary" disabled={d.changed.length === 0 || d.busy || hasClientErr} onClick={() => void save()}>
            {d.busy ? "Saving…" : "Save changes"}
          </Button>
        </div>
      </SaveBar>
    </div>
  );
}
