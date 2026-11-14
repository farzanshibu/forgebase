import { Route01Icon } from "@hugeicons/core-free-icons";
import { Link } from "react-router-dom";
import { getFeatures } from "../../api";
import type { FeatureSetting } from "../../api";
import { Badge } from "../../components/StatusBadge";
import { useLoad } from "../../hooks";
import { settingsAnchor } from "../setup/settingUtil";
import { Card, Row } from "./cards";

/**
 * Tracing export (ADR-0035): whether traces go to an OpenTelemetry collector, and where. The server reports it as the
 * `observability` feature's settings; nothing is drawn when the list is not available (a server without it, or no access).
 */
export default function TracingCard() {
  const features = useLoad(getFeatures, "system-features-tracing");
  const f = features.data?.features.find((x) => x.id === "observability");
  if (!f) return null;
  const get = (key: string): FeatureSetting | undefined => f.settings.find((s) => s.key === `observability.otlp.${key}`);
  const enabled = get("enabled")?.value === true;
  const endpoint = get("endpoint")?.value;
  const incoming = f.settings.find((s) => s.key === "observability.trust_incoming_trace")?.value;
  return (
    <Card title="Tracing" icon={Route01Icon}>
      <dl>
        <Row label="OTLP export" value={<Badge tone={enabled ? "ok" : "neutral"} dot={enabled}>{enabled ? "enabled" : "off"}</Badge>} />
        {enabled && typeof endpoint === "string" && endpoint && <Row label="Collector" value={endpoint} mono />}
        {enabled && typeof get("protocol")?.value === "string" && <Row label="Format" value={String(get("protocol")?.value)} />}
        {typeof incoming === "string" && <Row label="Continue callers' traces" value={incoming} />}
      </dl>
      <p className="mt-3 text-xs text-muted-foreground">
        {enabled ? "Responses carry a traceparent header and traces are sent to the collector." : "Nothing is exported and no trace header is added. Log lines still carry a trace id in JSON format."}{" "}
        <Link className="font-medium text-primary underline-offset-2 hover:underline" to={`/system/features#${settingsAnchor("observability")}`}>Change in Features and settings</Link>
      </p>
    </Card>
  );
}
