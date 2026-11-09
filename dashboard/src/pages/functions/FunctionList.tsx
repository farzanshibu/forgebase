import { useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { deleteFunction, listFunctions } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading } from "../../components/ui";
import { useLoad } from "../../hooks";
import DeployForm from "./DeployForm";
import { when } from "./util";

export default function FunctionList() {
  const list = useLoad(listFunctions, "functions");
  const nav = useNavigate();
  const [error, setError] = useState<unknown>();
  const [deploying, setDeploying] = useState(false);

  async function remove(name: string) {
    setError(undefined);
    try {
      await deleteFunction(name);
      list.reload();
    } catch (e) {
      setError(e);
    }
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <p className="text-sm text-zinc-400">Sandboxed WebAssembly functions of your tenant.</p>
        <Button tone="primary" onClick={() => setDeploying((d) => !d)}>{deploying ? "Cancel" : "Deploy function"}</Button>
      </div>
      {deploying && <DeployForm onDone={(d) => nav(`/functions/f/${encodeURIComponent(d.function.name)}`)} />}
      <ErrorView error={error ?? list.error} />
      {list.loading && !list.error && <Loading what="Loading functions" />}
      {list.data && list.data.length === 0 && <p className="text-sm text-zinc-400">No functions yet.</p>}
      {list.data && list.data.length > 0 && (
        <table className="w-full text-left text-sm">
          <thead className="text-xs uppercase tracking-wide text-zinc-500">
            <tr><th className="py-2">Name</th><th>Active</th><th>Latest</th><th>HTTP access</th><th>Updated</th><th /></tr>
          </thead>
          <tbody>
            {list.data.map((f) => (
              <tr key={f.name} className="border-t border-zinc-800">
                <td className="py-2">
                  <Link className="text-amber-300 hover:underline" to={`/functions/f/${encodeURIComponent(f.name)}`}>{f.name}</Link>
                  {f.description && <span className="ml-2 text-zinc-500">{f.description}</span>}
                </td>
                <td>v{f.active_version}</td>
                <td>v{f.latest_version}</td>
                <td>{f.access}</td>
                <td className="text-zinc-400">{when(f.updated_at)}</td>
                <td className="text-right"><ConfirmButton label={`Delete ${f.name}`} onConfirm={() => void remove(f.name)} /></td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
