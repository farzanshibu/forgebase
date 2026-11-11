/**
 * What the signed-in administrator manages (FB-137): the whole server (`platform`) or one realm. A realm administrator
 * gets the identity studios for their own realm; screens that change the server itself are for platform administrators.
 *
 * Usage:
 *   const { platform, realm } = useAdminScope();
 *   {platform && <Button>New realm</Button>}
 *   <PlatformOnly what="Client apps"><ClientsPage /></PlatformOnly>      // a realm administrator sees an explanation instead
 *
 * Outside a SessionProvider (and for a server older than FB-137, which sends no `admin_scope`) the scope is `platform`.
 */
import type { ReactNode } from "react";
import { EmptyState } from "../components/EmptyState";
import { useOptionalSession } from "./session";

export interface AdminScopeInfo {
  /** True for a platform administrator: the whole server. */
  platform: boolean;
  /** The realm a realm administrator manages; undefined for a platform administrator. */
  realm?: string;
}

export function useAdminScope(): AdminScopeInfo {
  const scope = useOptionalSession()?.adminScope;
  if (scope && typeof scope === "object") return { platform: false, realm: scope.realm };
  return { platform: true };
}

/** Shows `children` to a platform administrator; a realm administrator gets a short explanation instead of a 403. */
export function PlatformOnly({ what, children }: { what: string; children: ReactNode }) {
  const { platform, realm } = useAdminScope();
  if (platform) return <>{children}</>;
  return (
    <EmptyState
      title={`${what} is for platform administrators`}
      why={<>You manage the realm <code>{realm}</code>. This part changes the whole server, so ask a platform administrator to do it.</>}
    />
  );
}
