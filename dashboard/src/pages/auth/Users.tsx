import { Search01Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { Input } from "panelui-web/input";
import { Table } from "panelui-web/table";
import { useEffect, useMemo, useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { authAudit, listCollections, listSuspensions, searchUsers } from "../../api";
import { Sel } from "../../components/controls";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { Pager } from "../../components/Pager";
import { SkeletonTable } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import { Person } from "./common";
import { Badge } from "../../components/StatusBadge";
import { CardBody, HEAD_ROW, TableCard, Th } from "../../components/TableCard";
import { IdText } from "../../components/IdText";
import { When } from "../../components/Time";

/** Audit events that mean "this person signed in". */
const SIGN_INS = new Set(["login", "magic_link", "oauth.login", "webauthn.login", "anonymous"]);
const RECENT = 500;

const userHref = (collection: string, id: string) => `/auth/users/${encodeURIComponent(collection)}/${encodeURIComponent(id)}`;

export default function Users() {
  const navigate = useNavigate();
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
  const susp = useLoad(listSuspensions, "suspensions");
  // The last sign-in of each user, from the newest audit events (one request, not one per row).
  const audit = useLoad((s) => authAudit({ limit: RECENT }, s), "users:recent-sign-ins");
  const lastSignIn = useMemo(() => {
    const m = new Map<string, string>();
    for (const e of audit.data ?? []) {
      if (e.outcome !== "success" || !SIGN_INS.has(e.event) || !e.subject_id) continue;
      const k = `${e.collection ?? ""}/${e.subject_id}`;
      if (!m.has(k)) m.set(k, e.at);
    }
    return m;
  }, [audit.data]);
  const suspended = useMemo(() => new Set((susp.data ?? []).map((s) => s.principal)), [susp.data]);

  // Search as you type, after a short pause; Enter searches at once.
  useEffect(() => {
    const t = setTimeout(() => {
      const q = input.trim();
      if (q !== search) {
        setSearch(q);
        setPage(1);
      }
    }, 300);
    return () => clearTimeout(t);
  }, [input, search]);

  const d = list.data;
  const total = d?.total_items ?? d?.items.length;
  const loading = (cols.loading || list.loading) && !cols.error && !list.error;

  if (!cols.loading && !cols.error && authCols.length === 0) {
    return (
      <EmptyState
        title="No user accounts yet"
        why="Users sign in to your app. Make a collection of type Auth in the Database studio and its records become accounts that can sign in with a password, a link or a provider."
        action={<Link to="/database/new" className="text-sm font-medium text-primary underline-offset-2 hover:underline">Create an Auth collection</Link>}
        docs="docs/adr/0004-auth.md"
      />
    );
  }

  return (
    <TableCard
      title="Users"
      count={total ?? undefined}
      description="People who signed up to your app. Open one to see their sessions, sign-in history and suspension."
      toolbar={
        <form
          role="search"
          className="flex w-full flex-wrap items-center gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            setPage(1);
            setSearch(input.trim());
          }}
        >
          <Input.Group size="sm" className="w-full max-w-sm flex-1">
            <Input.Addon>
              <HugeiconsIcon icon={Search01Icon} size={14} aria-hidden="true" />
            </Input.Addon>
            <Input type="search" aria-label="Search users" placeholder="Search by e-mail or name" value={input} onChange={(e) => setInput(e.target.value)} />
          </Input.Group>
          {authCols.length > 1 && (
            <span className="flex items-center gap-2 text-xs text-muted-foreground">
              Collection
              <Sel
                aria-label="Auth collection"
                value={collection ?? ""}
                onChange={(v) => {
                  setPicked(v);
                  setPage(1);
                }}
                options={authCols}
              />
            </span>
          )}
          {authCols.length === 1 && (
            <span className="text-xs text-muted-foreground">
              in <code className="font-mono text-foreground/80">{collection}</code>
            </span>
          )}
        </form>
      }
      footer={d && d.items.length > 0 && collection ? <Pager page={d.page ?? page} perPage={d.per_page ?? 25} total={d.total_items ?? undefined} onPage={setPage} loading={list.loading} label="Users pages" /> : undefined}
    >
      {!!(cols.error || list.error) && (
        <CardBody>
          <ErrorPanel error={cols.error ?? list.error} onRetry={() => { cols.reload(); list.reload(); }} />
        </CardBody>
      )}
      {loading && !d && (
        <CardBody>
          <SkeletonTable rows={5} columns={5} label="Loading users" />
        </CardBody>
      )}
      {d && d.items.length === 0 && (
        <CardBody>
          {search ? (
            <EmptyState compact title="No users match" why={<>Nobody in <code>{collection}</code> matches “{search}”. Clear the search to see every user.</>} />
          ) : (
            <EmptyState compact title="No users yet" why="Nobody has signed up to this collection yet. Accounts appear here as soon as someone signs up in your app." />
          )}
        </CardBody>
      )}
      {d && d.items.length > 0 && collection && (
        <Table framed aria-label="Users" className="text-sm">
          <Table.Header className={HEAD_ROW}>
            <Table.Row>
              <Th>User</Th>
              <Th>Status</Th>
              <Th>E-mail</Th>
              <Th>Last sign-in</Th>
              <Th>Created</Th>
            </Table.Row>
          </Table.Header>
          <Table.Body>
            {d.items.map((u) => {
              const id = String(u.id);
              const email = String(u.email ?? id);
              const isSuspended = suspended.has(`user:${collection}/${id}`);
              return (
                <Table.Row key={id} className="cursor-pointer" onClick={(e) => { if (!(e.target as HTMLElement).closest("a,button")) navigate(userHref(collection, id)); }}>
                  <Table.Cell className="max-w-80">
                    <Person name={email} sub={<IdText id={id} what="user id" />}>
                      <Link className="hover:text-primary hover:underline focus-visible:underline" to={userHref(collection, id)}>
                        {email}
                      </Link>
                    </Person>
                  </Table.Cell>
                  <Table.Cell>{isSuspended ? <Badge status="suspended">Suspended</Badge> : <Badge status="active">Active</Badge>}</Table.Cell>
                  <Table.Cell>{u.verified ? <Badge tone="ok">Verified</Badge> : <Badge tone="neutral">Unverified</Badge>}</Table.Cell>
                  <Table.Cell className="text-muted-foreground">
                    <When value={lastSignIn.get(`${collection}/${id}`)} fallback={<span title={`No sign-in among the latest ${RECENT} audit events`}>Not recently</span>} />
                  </Table.Cell>
                  <Table.Cell className="text-muted-foreground">
                    <When value={u.created as string | undefined} />
                  </Table.Cell>
                </Table.Row>
              );
            })}
          </Table.Body>
        </Table>
      )}
    </TableCard>
  );
}
