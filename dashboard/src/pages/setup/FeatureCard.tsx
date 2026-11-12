import { Link } from "react-router-dom";
import { ArrowRight01Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { Badge } from "panelui-web/badge";
import { Card } from "panelui-web/card";
import { Switch } from "panelui-web/switch";
import type { ServerFeature } from "../../api";
import { DocsLink } from "../../components/PageHeader";
import { ALWAYS_ON_TEXT, Hint } from "./Hint";
import { featureIcon } from "./featureIcons";
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
  const active = fixed || on;
  return (
    <Card
      className={`flex h-full flex-col transition-opacity ${active ? "" : "opacity-75"}`}
      data-feature={f.id}
      data-on={active ? "true" : "false"}
    >
      <Card.Header>
        <div className="flex items-start justify-between gap-3">
          <div className="flex min-w-0 items-center gap-2.5">
            <span
              className={`flex size-8 shrink-0 items-center justify-center rounded-lg border ${active ? "border-primary/25 bg-primary/10 text-primary" : "border-border bg-muted text-muted-foreground"}`}
              aria-hidden="true"
            >
              <HugeiconsIcon icon={featureIcon(f.id)} size={16} strokeWidth={1.8} />
            </span>
            <h3 className="truncate font-medium leading-tight">{f.name}</h3>
          </div>
          {fixed ? (
            <Hint text={ALWAYS_ON_TEXT} label="Always on. What does this mean?">
              <Badge variant="secondary" size="sm">always on</Badge>
            </Hint>
          ) : (
            <span className="flex shrink-0 items-center gap-2">
              {/* The word as well as the switch position: colour is never the only signal. */}
              <span aria-hidden="true" className={`text-xs font-medium ${on ? "text-success" : "text-muted-foreground"}`}>
                {on ? "On" : "Off"}
              </span>
              <Switch label={`${f.name} enabled`} checked={on} disabled={locked} onCheckedChange={(c) => es && onToggle(c)} />
            </span>
          )}
        </div>
        <Card.Description className="mt-1">{f.summary}</Card.Description>
      </Card.Header>
      <Card.Content className="mt-auto space-y-2 text-xs text-muted-foreground">
        {es && (es.source === "env" || es.source === "flag") && (
          <p className="flex items-center gap-1.5">
            <SourceBadge source={es.source} />
            {`Set by ${es.source === "env" ? "an environment variable" : "a command-line flag"}: read-only.`}
          </p>
        )}
        {error && <p role="alert" className="font-medium text-destructive">{f.name}: {error}</p>}
        {((f.studio_path && f.enabled) || f.docs) && (
          <p className="flex flex-wrap items-center gap-x-4 gap-y-1 border-t border-border pt-3">
            {f.studio_path && f.enabled && (
              <Link className="inline-flex items-center gap-1 font-medium text-primary underline-offset-2 hover:underline" to={f.studio_path}>
                Open {f.name} studio
                <HugeiconsIcon icon={ArrowRight01Icon} size={14} strokeWidth={2} aria-hidden="true" />
              </Link>
            )}
            {f.docs && <DocsLink doc={f.docs} label={`${f.name} docs`} />}
          </p>
        )}
      </Card.Content>
    </Card>
  );
}
