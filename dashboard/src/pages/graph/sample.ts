import { createEdge, createNode } from "../../api";

/** A small sample: four people and who knows whom. Ids are fixed so the sample reads the same in docs and snippets. */
export async function loadGraphSample(): Promise<void> {
  const people = [["alice", "Alice", "engineer"], ["bob", "Bob", "designer"], ["carol", "Carol", "engineer"], ["dave", "Dave", "manager"]] as const;
  for (const [id, name, role] of people) await createNode({ id, label: "person", properties: { name, role } });
  for (const [source, target] of [["alice", "bob"], ["bob", "carol"], ["alice", "carol"], ["dave", "alice"]] as const) {
    await createEdge({ type: "knows", source, target, directed: true, properties: { since: 2024 } });
  }
}
