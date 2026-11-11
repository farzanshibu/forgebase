/**
 * PgOnly: the PostgreSQL pages of the Database studio (Policies, Logins, Extensions, Top queries) say what they need
 * when the project runs on SQLite, instead of failing with a 400 (FB-155).
 *
 * Usage:  <PgOnly info={info} what="Direct logins">…the page…</PgOnly>
 */
import type { ReactNode } from "react";
import { EmptyState } from "../../components/EmptyState";
import { SkeletonTable } from "../../components/Skeleton";
import type { DatabaseInfo } from "./info";

export default function PgOnly({ info, what, children }: { info: DatabaseInfo | undefined; what: string; children: ReactNode }) {
  if (!info) return <SkeletonTable rows={3} columns={4} label={`Loading ${what.toLowerCase()}`} />;
  if (info.backend !== "postgres") {
    return (
      <EmptyState
        title={`${what} need PostgreSQL`}
        why={<>This project runs on SQLite. <code>forgebase db convert --to postgres</code> moves it, and this page then works.</>}
        docs="docs/database.md"
      />
    );
  }
  return <>{children}</>;
}
