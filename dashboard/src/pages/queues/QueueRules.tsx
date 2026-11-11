/**
 * Access rules of one topic or one subscription (FB-141): the shared rules editor from the Database studio (FB-100)
 * with the two queue kinds. Queue rules read `@request` only (who is calling): there are no item fields.
 */
import ResourceRules from "../../components/ResourceRules";

export default function QueueRules({ kind, name }: { kind: "queue_topic" | "queue_subscription"; name: string }) {
  return <ResourceRules kind={kind} name={name} noun={kind === "queue_topic" ? "topic" : "subscription"} />;
}
