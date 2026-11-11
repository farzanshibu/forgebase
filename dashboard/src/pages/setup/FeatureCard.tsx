import { Link } from "react-router-dom";
import { Badge } from "panelui-web/badge";
import { Card } from "panelui-web/card";
import { Switch } from "panelui-web/switch";
import type { ServerFeature } from "../../api";
import { DocsLink } from "../../components/PageHeader";
import { ALWAYS_ON_TEXT, Hint } from "./Hint";
import { SourceBadge } from "./SettingField";
import { enabledSetting, readOnlyReason } from "./settingUtil";

/**
 * One feature on the feature map: name, summary, an on/off switch (or "always on"), where its value comes from, a docs link and
 * a link to its studio once the feature is really on. The studio link follows the saved state (`f.enabled`), not an unsaved switch.
 */
export default function FeatureCard({ feature: f, on, error, onToggle }: {
  feature: ServerFeature;
  /** The switch position, including an unsaved edit. */
  on: boolean;
  error?: string;
  onToggle: (on: boolean) => void;
}) {
  const es = enabledSetting(f);
  const reason = es ? readOnlyReason(es) : null;
  const locked = !f.can_disable || !es || reason !== null || !es.editable;
  const fixed = f.always_on || !f.can_disable;
  return (
    <Card className="h-full" data-feature={f.id}>
      <Card.Header>
        <div className="flex items-center justify-between gap-2">
          <h3 className="font-medium leading-tight">{f.name}</h3>
          {fixed ? (
            <Hint text={ALWAYS_ON_TEXT} label="Always on. What does this mean?">
              <Badge variant="secondary" size="sm">always on</Badge>
            </Hint>
          ) : (
            <Switch label={`${f.name} enabled`} checked={on} disabled={locked} onCheckedChange={(c) => es && onToggle(c)} />
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
        {error && <p role="alert" className="font-medium text-destructive">{f.name}: {error}</p>}
        <p className="flex flex-wrap items-center gap-x-3 gap-y-1">
          {f.studio_path && f.enabled && (
            <Link className="text-primary underline-offset-2 hover:underline" to={f.studio_path}>
              Open {f.name} studio
            </Link>
          )}
          {f.docs && <DocsLink doc={f.docs} label={`${f.name} docs`} />}
        </p>
      </Card.Content>
    </Card>
  );
}
