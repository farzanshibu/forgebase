import { useState } from "react";
import { ApiError, patchConfig, type ConfigWritten, type FeatureSetting, type ServerFeature, type SettingValue } from "../../api";
import { enabledKey, isUnchanged } from "./settingUtil";

/**
 * The edits a person has made but not saved, shared by the first-run flow and the System form. `save()` sends one PATCH with
 * the changed keys only and keeps the draft when the server refuses.
 */
export function useSettingsDraft(features: ServerFeature[]) {
  const [draft, setDraft] = useState<Record<string, SettingValue>>({});
  const [clientErr, setClientErr] = useState<Record<string, string>>({});
  const [serverErr, setServerErr] = useState<Record<string, string>>({});
  const [saveErr, setSaveErr] = useState<unknown>();
  const [busy, setBusy] = useState(false);

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
  const discard = () => {
    setDraft({});
    setClientErr({});
    setServerErr({});
    setSaveErr(undefined);
  };

  /** Resolves with what the server wrote, or `{ fields }` (the keys it refused) when it refused; the errors are also in `serverErr` and `saveErr`. */
  async function save(): Promise<ConfigWritten | { fields: string[] }> {
    setBusy(true);
    setSaveErr(undefined);
    setServerErr({});
    try {
      const set: Record<string, SettingValue> = {};
      for (const k of changed) set[k] = draft[k]!;
      const r = await patchConfig(set);
      setDraft({});
      return r;
    } catch (e) {
      const refused = e instanceof ApiError && e.code === "validation_failed" ? e.details : {};
      if (Object.keys(refused).length > 0) setServerErr(refused);
      setSaveErr(e);
      return { fields: Object.keys(refused) };
    } finally {
      setBusy(false);
    }
  }

  void features;
  return { draft, changed, clientErr, serverErr, saveErr, busy, edit, valueOf, isOn, save, discard };
}
