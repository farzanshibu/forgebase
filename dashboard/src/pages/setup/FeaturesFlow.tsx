import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { Alert } from "panelui-web/alert";
import { Steps } from "panelui-web/steps";
import { CheckmarkCircle02Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import type { ServerFeature, FeaturesResponse } from "../../api";
import { ErrorPanel } from "../../components/ErrorPanel";
import { Button } from "../../components/ui";
import FeatureCard from "./FeatureCard";
import SettingsSections from "./SettingsSections";
import { enabledKey, enabledSetting } from "./settingUtil";
import { useSettingsDraft } from "./useSettingsDraft";

type Step = "map" | "configure" | "done";
const ORDER: Step[] = ["map", "configure", "done"];
const TITLES: Record<Step, string> = { map: "Features", configure: "Configure", done: "Done" };

/**
 * Steps 2-4 of first-run setup, also the "Features & settings" page: the feature map, the settings of the enabled features,
 * and the done step. Edits stay in a draft; Save sends one PATCH with the changed keys only.
 * `before` is the number of steps ahead of this flow (the wizard has the administrator step).
 */
export default function FeaturesFlow({ snapshot, apply, before = 0, onRestartPending }: {
  snapshot: FeaturesResponse;
  /** Take the features a save answered with. */
  apply: (features: ServerFeature[], restartPending?: boolean) => void;
  before?: number;
  onRestartPending?: () => void;
}) {
  const navigate = useNavigate();
  const [step, setStep] = useState<Step>("map");
  const d = useSettingsDraft();
  const [restart, setRestart] = useState(false);
  const [saved, setSaved] = useState<string[]>([]);
  const [written, setWritten] = useState<string>();

  const features = snapshot.features;

  async function save() {
    if (d.changed.length === 0) {
      setSaved([]);
      setRestart(snapshot.restart_pending);
      setStep("done");
      return;
    }
    const r = await d.save();
    if ("fields" in r) {
      // An error on a switch is shown on the map; the rest on the configure step.
      if (r.fields.length > 0 && r.fields.every((k) => k.endsWith(".enabled"))) setStep("map");
      return;
    }
    apply(r.features, r.restart_required ? true : undefined);
    setSaved(r.applied);
    setWritten(r.written);
    setRestart(r.restart_required || snapshot.restart_pending);
    if (r.restart_required) onRestartPending?.();
    setStep("done");
  }

  const sections = features.filter((f) => f.always_on || d.isOn(f));
  const visibleKeys = new Set(sections.flatMap((f) => f.settings.map((s) => s.key)));
  const elsewhere = Object.entries(d.serverErr).filter(([k]) => !visibleKeys.has(k) && !k.endsWith(".enabled"));
  const idx = ORDER.indexOf(step);
  const onCount = features.filter((f) => f.always_on || d.isOn(f)).length;
  // Next / Save stay in reach on a long step: the same sticky bar as Features and settings.
  const bar = "sticky bottom-0 z-10 -mx-1 flex items-center justify-between gap-3 border-t border-border bg-background/95 px-1 py-3 backdrop-blur";

  return (
    <div className="space-y-6">
      <Steps value={before + idx} aria-label="Setup steps">
        {before > 0 && (
          <Steps.Item step={0} completed disabled>
            <Steps.Trigger>
              <Steps.Indicator />
              <Steps.Title>Administrator</Steps.Title>
            </Steps.Trigger>
          </Steps.Item>
        )}
        {ORDER.map((s, i) => (
          <Steps.Item key={s} step={before + i} disabled={i > idx}>
            <Steps.Trigger onClick={() => i < idx && step !== "done" && setStep(s)}>
              <Steps.Indicator />
              <Steps.Title>{TITLES[s]}</Steps.Title>
            </Steps.Trigger>
          </Steps.Item>
        ))}
      </Steps>

      {snapshot.restart_pending && step !== "done" && (
        <Alert tone="warning" variant="plain">
          <Alert.Icon />
          <Alert.Title>A restart is already pending</Alert.Title>
          <Alert.Description>Earlier changes in {snapshot.config_file} take effect when the server restarts.</Alert.Description>
        </Alert>
      )}

      {step === "map" && (
        <section aria-label="Feature map" className="space-y-4">
          <div>
            <div className="flex flex-wrap items-baseline justify-between gap-2">
              <h2 className="text-lg font-semibold tracking-tight">Feature map</h2>
              <span className="text-sm text-muted-foreground tabular-nums">
                {onCount} of {features.length} on
              </span>
            </div>
            <p className="text-sm text-muted-foreground">Choose what this server runs. A feature that is off answers “disabled” and starts nothing.</p>
            <p className="text-sm text-muted-foreground" data-testid="database-engines-note">The database engine is chosen when the project is created: PostgreSQL (the default) or SQLite (one file). <code>forgebase db convert --to postgres</code> moves an SQLite project.</p>
          </div>
          <ErrorPanel error={Object.keys(d.serverErr).some((k) => k.endsWith(".enabled")) ? d.saveErr : undefined} />
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
          <div className={bar}>
            <span className="text-sm text-muted-foreground">You can change this later in System › Features and settings.</span>
            <Button tone="primary" onClick={() => setStep("configure")}>
              Next: configure
            </Button>
          </div>
        </section>
      )}

      {step === "configure" && (
        <section aria-label="Configure" className="space-y-4">
          <div>
            <h2 className="text-lg font-semibold tracking-tight">Configure</h2>
            <p className="text-sm text-muted-foreground">
              Settings of the enabled features. Saving writes only what you changed to <code>{snapshot.config_file}</code>.
            </p>
          </div>
          <SettingsSections features={sections} valueOf={d.valueOf} errorOf={(k) => d.clientErr[k] ?? d.serverErr[k]} onEdit={d.edit} />
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
          <div className={bar}>
            <Button onClick={() => setStep("map")}>Back</Button>
            <div className="flex items-center gap-3">
              <span className="text-xs text-muted-foreground" aria-live="polite">
                {d.changed.length === 0 ? "No changes" : `${d.changed.length} changed`}
              </span>
              <Button tone="primary" disabled={d.busy || Object.keys(d.clientErr).length > 0} onClick={() => void save()}>
                {d.changed.length === 0 ? "Finish" : "Save and finish"}
              </Button>
            </div>
          </div>
        </section>
      )}

      {step === "done" && (
        <section aria-label="Done" className="mx-auto max-w-2xl space-y-5 rounded-2xl border border-border bg-card p-6 shadow-sm md:p-8">
          <div className="flex items-center gap-3">
            <span className="flex size-10 shrink-0 items-center justify-center rounded-full bg-success/10 text-success ring-4 ring-success/5" aria-hidden="true">
              <HugeiconsIcon icon={CheckmarkCircle02Icon} size={22} strokeWidth={2} />
            </span>
            <div>
              <h2 className="text-lg font-semibold tracking-tight">Done</h2>
              <p className="text-sm text-muted-foreground">ForgeBase is set up. {onCount} of {features.length} features are on.</p>
            </div>
          </div>
          {saved.length > 0 ? (
            <p className="text-sm">
              Saved {saved.length === 1 ? "1 setting" : `${saved.length} settings`} to <code>{written ?? snapshot.config_file}</code>: {saved.join(", ")}.
            </p>
          ) : (
            <p className="text-sm">Nothing changed, so nothing was written.</p>
          )}
          {restart ? (
            <Alert tone="warning" variant="plain">
              <Alert.Icon />
              <Alert.Title>Restart the server to finish</Alert.Title>
              <Alert.Description>Stop and run <code>forgebase serve</code> again, or restart the service with your service manager (for example <code>systemctl restart forgebase</code>).</Alert.Description>
            </Alert>
          ) : (
            <Alert tone="success" variant="plain">
              <Alert.Icon />
              <Alert.Title>No restart needed</Alert.Title>
              <Alert.Description>The changes are already in effect.</Alert.Description>
            </Alert>
          )}
          <div className="flex gap-2">
            <Button tone="primary" type="button" onClick={() => navigate("/")}>
              Go to Overview
            </Button>
            <Button
              onClick={() => {
                setStep("map");
                setSaved([]);
              }}
            >
              Change more settings
            </Button>
          </div>
        </section>
      )}
    </div>
  );
}
