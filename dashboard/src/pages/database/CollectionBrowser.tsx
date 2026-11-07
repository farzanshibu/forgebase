import { Link } from "react-router-dom";
import { listCollections } from "../../api";
import { useLoad } from "../../hooks";
import { ErrorView, Loading } from "../../components/ui";

export default function CollectionBrowser() {
  const { data, error, loading } = useLoad(listCollections, "collections");
  return (
    <div>
      <div className="mb-4 flex items-center justify-between">
        <p className="text-sm text-zinc-400">Every collection the server knows about.</p>
        <Link to="/database/new" className="rounded-md border border-amber-500 bg-amber-500 px-3 py-1.5 text-sm text-zinc-950 hover:bg-amber-400">
          New collection
        </Link>
      </div>
      <ErrorView error={error} />
      {loading && !error && <Loading what="Loading collections" />}
      {data && data.length === 0 && <p className="text-sm text-zinc-400">No collections yet. Create one to start storing records.</p>}
      {data && data.length > 0 && (
        <table className="w-full text-left text-sm">
          <thead className="text-xs uppercase tracking-wide text-zinc-500">
            <tr>
              <th className="py-2">Name</th>
              <th>Type</th>
              <th>Mode</th>
              <th>Fields</th>
              <th>Version</th>
            </tr>
          </thead>
          <tbody>
            {data.map((c) => (
              <tr key={c.name} className="border-t border-zinc-800">
                <td className="py-2">
                  <Link className="text-amber-300 hover:underline" to={`/database/c/${encodeURIComponent(c.name)}`}>
                    {c.name}
                  </Link>
                  {c.system && <span className="ml-2 text-xs text-zinc-500">system</span>}
                </td>
                <td>{c.type}</td>
                <td>{c.schema_mode}</td>
                <td>{c.fields.length}</td>
                <td>{c.version}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
