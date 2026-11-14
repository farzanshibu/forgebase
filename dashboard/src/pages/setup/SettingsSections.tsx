import { Card } from "panelui-web/card";
import { HugeiconsIcon } from "@hugeicons/react";
import { Link } from "react-router-dom";
import type { FeatureSetting, ServerFeature, SettingValue } from "../../api";
import SettingField from "./SettingField";
import { featureIcon } from "./featureIcons";
import { groupSettings, settingsAnchor } from "./settingUtil";
import StorageDriverGuide from "./StorageDriverGuide";
import { driverOf } from "./storageDrivers";

const settingCount = (groups: { rows: unknown[] }[]) => {
  const n = groups.reduce((sum, g) => sum + g.rows.length, 0);
  return n === 1 ? "1 setting" : `${n} settings`;
};

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
          <Card key={f.id} id={settingsAnchor(f.id)} aria-label={`${f.name} settings`} className="scroll-mt-6">
            <Card.Header className="flex flex-row items-center gap-2.5">
              <span className="flex size-7 shrink-0 items-center justify-center rounded-md border border-border bg-muted text-muted-foreground" aria-hidden="true">
                <HugeiconsIcon icon={featureIcon(f.id)} size={15} strokeWidth={1.8} />
              </span>
              <h3 className="min-w-0 flex-1 font-medium">{f.name}</h3>
              <span className="text-xs text-muted-foreground tabular-nums">{settingCount(groups)}</span>
            </Card.Header>
            <Card.Content className="space-y-2">
              {groups.map((g) => (
                <section key={g.title ?? ""} aria-label={g.title ? `${f.name}: ${g.title}` : undefined} className="divide-y divide-border">
                  {g.title && <h4 className="pt-2 text-sm font-semibold tracking-tight">{g.title}</h4>}
                  {f.id === "database" && (g.title === "Read replicas" || g.title === "Sharding" || g.title === "Servers declared in the config file") && (
                    <p className="pt-1 text-xs text-muted-foreground">These settings change how replicas and shards behave. Add, promote and move the servers themselves in <Link to="/system/topology" className="text-primary hover:underline">System › Database topology</Link>.</p>
                  )}
                  {f.id === "storage" && g.rows[0] && driverOf(g.rows[0].key) && (
                    <p className="pt-1 text-xs text-muted-foreground">Used when the storage driver is <code className="font-mono">{driverOf(g.rows[0].key)}</code>{String(valueOf(f.settings.find((x) => x.key === "storage.driver") ?? g.rows[0])) === driverOf(g.rows[0].key) ? "." : ": it is not the selected driver, so these are not used now."}</p>
                  )}
                  {g.rows.map((s) => (
                    <SettingField key={s.key} setting={s} value={valueOf(s)} error={errorOf(s.key)} onChange={(v, e) => onEdit(s, v, e)} />
                  ))}
                </section>
              ))}
              {f.id === "storage" && f.settings.some((x) => x.key === "storage.driver") && (
                <StorageDriverGuide feature={f} driver={valueOf(f.settings.find((x) => x.key === "storage.driver")!)} />
              )}
            </Card.Content>
          </Card>
        );
      })}
    </>
  );
}
