import { useState } from "react";
import { Link } from "react-router-dom";
import { listCollections, searchUsers } from "../../api";
import { ErrorView, Loading, Button } from "../../components/ui";
import { Sel } from "../../components/controls";
import { useLoad } from "../../hooks";
import { THEAD, Th } from "./common";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";

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
        <label className="text-sm text-muted-foreground">
          Collection{" "}
          <Sel
            aria-label="Auth collection"
            value={collection ?? ""}
            onChange={(v) => {
              setPicked(v);
              setPage(1);
            }}
            options={authCols}
          />
        </label>
        <Input size="sm" aria-label="Search users" className="w-64" placeholder="Search email or name" value={input} onChange={(e) => setInput(e.target.value)} />
        <Button type="submit">Search</Button>
      </form>
      <ErrorView error={cols.error ?? list.error} />
      {(cols.loading || list.loading) && !cols.error && !list.error && <Loading what="Loading users" />}
      {!cols.loading && !cols.error && authCols.length === 0 && <p className="text-sm text-muted-foreground">No auth collections exist yet.</p>}
      {d && d.items.length === 0 && <p className="text-sm text-muted-foreground">No users match.</p>}
      {d && d.items.length > 0 && collection && (
        <>
          <Table className="w-full text-sm">
            <Table.Header className={THEAD}>
              <Table.Row>
                <Th>Email</Th>
                <Th>Id</Th>
                <Th>Verified</Th>
                <Th>Created</Th>
              </Table.Row>
            </Table.Header>
            <Table.Body>
              {d.items.map((u) => (
                <Table.Row key={String(u.id)} className="border-t border-border">
                  <Table.Cell className="py-2 pr-3">
                    <Link className="text-primary hover:underline" to={`/auth/users/${encodeURIComponent(collection)}/${encodeURIComponent(String(u.id))}`}>
                      {String(u.email ?? u.id)}
                    </Link>
                  </Table.Cell>
                  <Table.Cell className="pr-3 font-mono text-xs text-muted-foreground">{String(u.id)}</Table.Cell>
                  <Table.Cell className="pr-3">{u.verified ? "yes" : "no"}</Table.Cell>
                  <Table.Cell className="pr-3 text-muted-foreground">{String(u.created ?? "")}</Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
          <div className="mt-3 flex items-center gap-2 text-sm text-muted-foreground">
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
