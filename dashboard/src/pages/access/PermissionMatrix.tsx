import { catalogue, allKeys, fromCells, keyOf, NEXT, toCells, type Cell } from "./catalogue";
import { Table } from "panelui-web/table";

const LABEL: Record<Cell, string> = { inherit: "–", allow: "Allow", deny: "Deny" };

/** Tri-state matrix over the `resource:action` catalogue. Emits the role's allow/deny lists on every toggle. */
export default function PermissionMatrix({
  allow, deny, collections, readOnly, onChange,
}: {
  allow: string[]; deny: string[]; collections: string[]; readOnly?: boolean;
  onChange: (allow: string[], deny: string[]) => void;
}) {
  const rows = catalogue(collections);
  const { cells, customAllow, customDeny } = toCells(allow, deny, allKeys(rows));
  const columns = [...new Set(rows.flatMap((r) => r.actions))];
  const toggle = (key: string) => {
    const next = { ...cells, [key]: NEXT[cells[key] ?? "inherit"] };
    const out = fromCells(next, customAllow, customDeny);
    onChange(out.allow, out.deny);
  };
  return (
    <div>
      <div className="overflow-x-auto">
        <Table aria-label="Permission matrix" className="text-left text-xs">
          <Table.Header className="text-muted-foreground">
            <Table.Row>
              <Table.Head className="py-1 pr-3">Resource</Table.Head>
              {columns.map((c) => <Table.Head key={c} className="px-1 font-normal">{c}</Table.Head>)}
            </Table.Row>
          </Table.Header>
          <Table.Body>
            {rows.map((r) => (
              <Table.Row key={r.resource} className="border-t border-border">
                <Table.Head className="py-1 pr-3 font-mono font-normal text-foreground/80">{r.resource}</Table.Head>
                {columns.map((c) => {
                  if (!r.actions.includes(c)) return <Table.Cell key={c} />;
                  const key = keyOf(r.resource, c);
                  const state = cells[key] ?? "inherit";
                  return (
                    <Table.Cell key={c} className="px-1 py-0.5">
                      <button
                        type="button"
                        disabled={readOnly}
                        aria-label={`${key}: ${state}`}
                        data-state={state}
                        onClick={() => toggle(key)}
                        className={`w-14 rounded border px-1 py-0.5 ${
                          state === "deny" ? "border-destructive/40 bg-destructive/15 text-destructive"
                          : state === "allow" ? "border-success/40 bg-success/15 text-success"
                          : "border-border text-muted-foreground hover:border-foreground/25"
                        }`}
                      >
                        {LABEL[state]}
                      </button>
                    </Table.Cell>
                  );
                })}
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      </div>
      {(customAllow.length > 0 || customDeny.length > 0) && (
        <div aria-label="Custom patterns" className="mt-3 text-xs text-muted-foreground">
          <p className="mb-1">Custom patterns (kept as they are):</p>
          <ul className="flex flex-wrap gap-2">
            {customAllow.map((p) => <li key={`a${p}`} className="rounded bg-success/15 px-1.5 py-0.5 font-mono text-success">allow {p}</li>)}
            {customDeny.map((p) => <li key={`d${p}`} className="rounded bg-destructive/15 px-1.5 py-0.5 font-mono text-destructive">deny {p}</li>)}
          </ul>
        </div>
      )}
    </div>
  );
}
