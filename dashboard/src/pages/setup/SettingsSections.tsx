import { Card } from "panelui-web/card";
import type { FeatureSetting, ServerFeature, SettingValue } from "../../api";
import SettingField from "./SettingField";
import { groupSettings } from "./settingUtil";

/** One card per feature with its settings (the on/off switch lives on the feature map), grouped by what they configure. */
export default function SettingsSections({ features, valueOf, errorOf, onEdit }: {
  features: ServerFeature[];
  valueOf: (s: FeatureSetting) => SettingValue;
  errorOf: (key: string) => string | undefined;
  onEdit: (s: FeatureSetting, value: SettingValue, clientError?: string) => void;
}) {
  return (
    <>
      {features.map((f) => {
        const groups = groupSettings(f);
        if (groups.length === 0) return null;
        return (
          <Card key={f.id} aria-label={`${f.name} settings`}>
            <Card.Header>
              <h3 className="font-medium">{f.name}</h3>
            </Card.Header>
            <Card.Content className="space-y-2">
              {groups.map((g) => (
                <section key={g.title ?? ""} aria-label={g.title ? `${f.name}: ${g.title}` : undefined} className="divide-y divide-border">
                  {g.title && <h4 className="pt-2 text-sm font-semibold tracking-tight">{g.title}</h4>}
                  {g.rows.map((s) => (
                    <SettingField key={s.key} setting={s} value={valueOf(s)} error={errorOf(s.key)} onChange={(v, e) => onEdit(s, v, e)} />
                  ))}
                </section>
              ))}
            </Card.Content>
          </Card>
        );
      })}
    </>
  );
}
