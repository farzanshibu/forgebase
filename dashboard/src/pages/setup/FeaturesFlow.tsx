import { useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { Alert } from "panelui-web/alert";
import { Badge } from "panelui-web/badge";
import { Card } from "panelui-web/card";
import { Steps } from "panelui-web/steps";
import { Switch } from "panelui-web/switch";
import { ApiError, patchConfig, type ServerFeature, type FeatureSetting, type FeaturesResponse, type SettingValue } from "../../api";
import { Button, ErrorView } from "../../components/ui";
import SettingField, { SourceBadge } from "./SettingField";
import { isUnchanged, readOnlyReason } from "./settingUtil";

const enabledKey = (f: ServerFeature) => `${f.id}.enabled`;

/** The feature's own on/off setting, when it has one. */
const enabledSetting = (f: ServerFeature): FeatureSetting | undefined => f.settings.find((s) => s.key === enabledKey(f));

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
  const [draft, setDraft] = useState<Record<string, SettingValue>>({});
  const [clientErr, setClientErr] = useState<Record<string, string>>({});
  const [serverErr, setServerErr] = useState<Record<string, string>>({});
  const [saveErr, setSaveErr] = useState<unknown>();
  const [busy, setBusy] = useState(false);
  const [restart, setRestart] = useState(false);
  const [saved, setSaved] = useState<string[]>([]);
  const [written, setWritten] = useState<string>();

  const features = snapshot.features;

  function edit(s: FeatureSetting, value: SettingValue, err?: string) {
    setDraft((d) => {
      const next = { ...d };
      if (isUnchanged(s, value)) delete next[s.key];
      else next[s.key] = value;
      return next;
    });
    setClientErr((c) => {
      const next = { ...c };
      if (err) next[s.key] = err;
      else delete next[s.key];
      return next;
    });
    setServerErr((e) => {
      if (!(s.key in e)) return e;
      const next = { ...e };
      delete next[s.key];
      return next;
    });
  }

  const valueOf = (s: FeatureSetting): SettingValue => (s.key in draft ? draft[s.key]! : s.value);
  const isOn = (f: ServerFeature) => {
    const k = enabledKey(f);
    return k in draft ? draft[k] === true : f.enabled;
  };
  const changed = Object.keys(draft);

  async function save() {
    setBusy(true);
    setSaveErr(undefined);
    setServerErr({});
    try {
      if (changed.length === 0) {
        setSaved([]);
        setRestart(snapshot.restart_pending);
        setStep("done");
        return;
      }
      const set: Record<string, SettingValue> = {};
      for (const k of changed) set[k] = draft[k]!;
      const r = await patchConfig(set);
      apply(r.features, r.restart_required ? true : undefined);
      setSaved(r.applied);
      setWritten(r.written);
      setRestart(r.restart_required || snapshot.restart_pending);
      if (r.restart_required) onRestartPending?.();
      setDraft({});
      setStep("done");
    } catch (e) {
      if (e instanceof ApiError && e.code === "validation_failed" && Object.keys(e.details).length > 0) {
        setServerErr(e.details);
        // An error on a switch is shown on the map; the rest on the configure step.
        const keys = Object.keys(e.details);
        if (keys.some((k) => k.endsWith(".enabled")) && !keys.some((k) => !k.endsWith(".enabled"))) setStep("map");
      }
      setSaveErr(e);
    } finally {
      setBusy(false);
    }
  }

  const sections = features.filter((f) => f.always_on || isOn(f));
  const visibleKeys = new Set(sections.flatMap((f) => f.settings.map((s) => s.key)));
  const elsewhere = Object.entries(serverErr).filter(([k]) => !visibleKeys.has(k) && !k.endsWith(".enabled"));
  const idx = ORDER.indexOf(step);

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
            <h2 className="text-lg font-semibold tracking-tight">Feature map</h2>
            <p className="text-sm text-muted-foreground">Choose what this server runs. A feature that is off answers “disabled” and starts nothing.</p>
          </div>
          <ErrorView error={Object.keys(serverErr).some((k) => k.endsWith(".enabled")) ? saveErr : undefined} />
          <ul className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
            {features.map((f) => {
              const es = enabledSetting(f);
              const reason = es ? readOnlyReason(es) : null;
              const locked = !f.can_disable || !es || reason !== null || !es.editable;
              const on = f.always_on ? true : isOn(f);
              const err = serverErr[enabledKey(f)];
              return (
                <li key={f.id}>
                  <Card className="h-full" data-feature={f.id}>
                    <Card.Header>
                      <div className="flex items-center justify-between gap-2">
                        <h3 className="font-medium leading-tight">{f.name}</h3>
                        {f.always_on || !f.can_disable ? (
                          <Badge variant="secondary" size="sm">always on</Badge>
                        ) : (
                          <Switch label={`${f.name} enabled`} checked={on} disabled={locked} onCheckedChange={(c) => es && edit(es, c)} />
                        )}
                      </div>
                      <Card.Description>{f.summary}</Card.Description>
                    </Card.Header>
                    <Card.Content className="space-y-1.5 text-xs text-muted-foreground">
                      {es && (
                        <p className="flex items-center gap-1.5">
                          <SourceBadge source={es.source} />
                          {es.source === "env" || es.source === "flag" ? `Set by ${es.source === "env" ? "an environment variable" : "a command-line flag"}: read-only.` : null}
                        </p>
                      )}
                      {err && <p role="alert" className="font-medium text-destructive">{f.name}: {err}</p>}
                      {f.studio_path && on && (
                        <Link className="text-primary underline-offset-2 hover:underline" to={f.studio_path}>
                          Open {f.name} studio
                        </Link>
                      )}
                    </Card.Content>
                  </Card>
                </li>
              );
            })}
          </ul>
          <div className="flex justify-end">
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
          {sections.map((f) => {
            const rows = f.settings.filter((s) => s.key !== enabledKey(f));
            if (rows.length === 0) return null;
            return (
              <Card key={f.id} aria-label={`${f.name} settings`}>
                <Card.Header>
                  <h3 className="font-medium">{f.name}</h3>
                </Card.Header>
                <Card.Content className="divide-y divide-border">
                  {rows.map((s) => (
                    <SettingField key={s.key} setting={s} value={valueOf(s)} error={clientErr[s.key] ?? serverErr[s.key]} onChange={(v, e) => edit(s, v, e)} />
                  ))}
                </Card.Content>
              </Card>
            );
          })}
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
          {saveErr !== undefined && Object.keys(serverErr).length === 0 && <ErrorView error={saveErr} />}
          <div className="flex items-center justify-between gap-2">
            <Button onClick={() => setStep("map")}>Back</Button>
            <div className="flex items-center gap-3">
              <span className="text-xs text-muted-foreground" aria-live="polite">
                {changed.length === 0 ? "No changes" : `${changed.length} changed`}
              </span>
              <Button tone="primary" disabled={busy || Object.keys(clientErr).length > 0} onClick={() => void save()}>
                {changed.length === 0 ? "Finish" : "Save and finish"}
              </Button>
            </div>
          </div>
        </section>
      )}

      {step === "done" && (
        <section aria-label="Done" className="space-y-4">
          <h2 className="text-lg font-semibold tracking-tight">Done</h2>
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
