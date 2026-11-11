/**
 * Access rules of one topic or one subscription (FB-141): the shared rules editor from the Database studio (FB-100)
 * with the two queue kinds. Queue rules read `@request` only (who is calling): there are no item fields.
 * The shared editor's kind list does not name these two yet, hence the cast; the server accepts them.
 */
import ResourceRules from "../database/ResourceRules";
import type { RuleKind } from "../database/rules";

export default function QueueRules({ kind, name }: { kind: "queue_topic" | "queue_subscription"; name: string }) {
  return <ResourceRules kind={kind as unknown as RuleKind} name={name} noun={kind === "queue_topic" ? "topic" : "subscription"} />;
}
