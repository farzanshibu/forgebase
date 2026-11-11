import ResourceRules from "../database/ResourceRules";

/** Namespace rules (FB-138): who may read, write or administer keys in this namespace. Rules see `@request` only; keys have no item fields. */
export default function Rules({ ns }: { ns: string }) {
  const shared = ns.startsWith("shared.");
  return (
    <div className="space-y-3">
      <p className="text-sm text-muted-foreground">
        {shared
          ? <>Namespace <code className="font-mono">{ns}</code> is shared: every tenant of a realm reads and writes the same keys, so rules here decide who may.</>
          : <>A caller with no tenant can only use a <code className="font-mono">shared.</code> namespace; a tenant-bound key or a request with a tenant uses its own copy of <code className="font-mono">{ns}</code>.</>}
        {" "}Reads map to list and view, writes to create and update, and administration to delete.
      </p>
      <ResourceRules kind="kv_namespace" name={ns} noun="namespace" />
    </div>
  );
}
