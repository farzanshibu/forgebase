import { useState } from "react";
import { Link } from "react-router-dom";
import { listCollections, searchUsers } from "../../api";
import { ErrorView, Loading, inputCls, Button } from "../../components/ui";
import { useLoad } from "../../hooks";
import { THEAD, Th } from "./common";

export default function Users() {
  const cols = useLoad(listCollections, "collections");
  const authCols = (cols.data ?? []).filter((c) => c.type === "auth").map((c) => c.name);
  const [picked, setPicked] = useState<string>();
  const collection = picked ?? (authCols.includes("users") ? "users" : authCols[0]);
  const [input, setInput] = useState("");
  const [search, setSearch] = useState("");
  const [page, setPage] = useState(1);
  const list = useLoad(
    (s) => (collection ? searchUsers(collection, { search, page }, s) : Promise.resolve(undefined)),
    `users:${collection}:${search}:${page}`,
  );
  const d = list.data;
  return (
    <div>
      <form
        className="mb-4 flex flex-wrap items-center gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          setPage(1);
          setSearch(input.trim());
        }}
      >
        <label className="text-sm text-zinc-400">
          Collection{" "}
          <select
            aria-label="Auth collection"
            className={inputCls}
            value={collection ?? ""}
            onChange={(e) => {
              setPicked(e.target.value);
              setPage(1);
            }}
          >
            {authCols.map((c) => (
              <option key={c}>{c}</option>
            ))}
          </select>
        </label>
        <input aria-label="Search users" className={`${inputCls} w-64`} placeholder="Search email or name" value={input} onChange={(e) => setInput(e.target.value)} />
        <Button type="submit">Search</Button>
      </form>
      <ErrorView error={cols.error ?? list.error} />
      {(cols.loading || list.loading) && !cols.error && !list.error && <Loading what="Loading users" />}
      {!cols.loading && !cols.error && authCols.length === 0 && <p className="text-sm text-zinc-400">No auth collections exist yet.</p>}
      {d && d.items.length === 0 && <p className="text-sm text-zinc-400">No users match.</p>}
      {d && d.items.length > 0 && collection && (
        <>
          <table className="w-full text-sm">
            <thead className={THEAD}>
              <tr>
                <Th>Email</Th>
                <Th>Id</Th>
                <Th>Verified</Th>
                <Th>Created</Th>
              </tr>
            </thead>
            <tbody>
              {d.items.map((u) => (
                <tr key={String(u.id)} className="border-t border-zinc-800">
                  <td className="py-2 pr-3">
                    <Link className="text-amber-300 hover:underline" to={`/auth/users/${encodeURIComponent(collection)}/${encodeURIComponent(String(u.id))}`}>
                      {String(u.email ?? u.id)}
                    </Link>
                  </td>
                  <td className="pr-3 font-mono text-xs text-zinc-400">{String(u.id)}</td>
                  <td className="pr-3">{u.verified ? "yes" : "no"}</td>
                  <td className="pr-3 text-zinc-400">{String(u.created ?? "")}</td>
                </tr>
              ))}
            </tbody>
          </table>
          <div className="mt-3 flex items-center gap-2 text-sm text-zinc-400">
            <Button disabled={page <= 1} onClick={() => setPage(page - 1)}>
              Previous
            </Button>
            <span>
              Page {d.page ?? page} of {d.total_pages ?? 1}
            </span>
            <Button disabled={page >= (d.total_pages ?? 1)} onClick={() => setPage(page + 1)}>
              Next
            </Button>
          </div>
        </>
      )}
    </div>
  );
}
