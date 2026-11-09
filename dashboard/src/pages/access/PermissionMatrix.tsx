import { catalogue, allKeys, fromCells, keyOf, NEXT, toCells, type Cell } from "./catalogue";

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
        <table aria-label="Permission matrix" className="text-left text-xs">
          <thead className="text-zinc-500">
            <tr>
              <th className="py-1 pr-3">Resource</th>
              {columns.map((c) => <th key={c} className="px-1 font-normal">{c}</th>)}
            </tr>
          </thead>
          <tbody>
            {rows.map((r) => (
              <tr key={r.resource} className="border-t border-zinc-800">
                <th className="py-1 pr-3 font-mono font-normal text-zinc-300">{r.resource}</th>
                {columns.map((c) => {
                  if (!r.actions.includes(c)) return <td key={c} />;
                  const key = keyOf(r.resource, c);
                  const state = cells[key] ?? "inherit";
                  return (
                    <td key={c} className="px-1 py-0.5">
                      <button
                        type="button"
                        disabled={readOnly}
                        aria-label={`${key}: ${state}`}
                        data-state={state}
                        onClick={() => toggle(key)}
                        className={`w-14 rounded border px-1 py-0.5 ${
                          state === "deny" ? "border-red-600 bg-red-500/25 text-red-200"
                          : state === "allow" ? "border-emerald-700 bg-emerald-500/15 text-emerald-200"
                          : "border-zinc-800 text-zinc-600 hover:border-zinc-600"
                        }`}
                      >
                        {LABEL[state]}
                      </button>
                    </td>
                  );
                })}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {(customAllow.length > 0 || customDeny.length > 0) && (
        <div aria-label="Custom patterns" className="mt-3 text-xs text-zinc-400">
          <p className="mb-1">Custom patterns (kept as they are):</p>
          <ul className="flex flex-wrap gap-2">
            {customAllow.map((p) => <li key={`a${p}`} className="rounded bg-emerald-500/15 px-1.5 py-0.5 font-mono text-emerald-200">allow {p}</li>)}
            {customDeny.map((p) => <li key={`d${p}`} className="rounded bg-red-500/25 px-1.5 py-0.5 font-mono text-red-200">deny {p}</li>)}
          </ul>
        </div>
      )}
    </div>
  );
}
