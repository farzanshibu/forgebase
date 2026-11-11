import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  ApiError, endImpersonation as apiEndImpersonation, impersonate as apiImpersonate, isMfaChallenge, login, logout, me, mfaVerify, userLogin,
  setCsrfToken, setTokenSource, setUnauthorizedHandler, type AdminInfo, type AdminScope, type LoginSession, type MeResponse,
} from "../api";

export interface ImpersonationState {
  /** `user:<collection>/<id>` */
  target: string;
  reason: string;
  sessionId: string;
  /** ISO time the impersonation token stops working. */
  expiresAt: string;
}

export interface SignInOptions {
  as?: "admin" | "user";
  /** Realm to sign in to when `as` is "user"; empty means the default realm. */
  realm?: string;
}

export interface Session {
  status: "loading" | "anonymous" | "authenticated";
  admin: AdminInfo | null;
  /** What the administrator may manage: the whole server (`platform`) or one realm (FB-137). `null` while signed out. */
  adminScope: AdminScope | null;
  /** A second factor is needed to finish `signIn`. */
  mfaRequired?: boolean;
  /** True after a 401 ended a signed-in session. */
  expiredNotice: boolean;
  /** `as: "user"` signs in as a user of `realm` (a realm administrator); the default is an instance administrator. */
  signIn: (email: string, password: string, opts?: SignInOptions) => Promise<void>;
  submitMfa: (code: string) => Promise<void>;
  signOut: () => Promise<void>;
  /** Take over a session another call created (the first-run setup answers like a sign-in). */
  adoptSession: (s: LoginSession) => Promise<void>;
  impersonation: ImpersonationState | null;
  startImpersonation: (principal: string, reason: string, ttlMinutes?: number) => Promise<void>;
  endImpersonation: () => Promise<void>;
}

const Ctx = createContext<Session | null>(null);

export function useSession(): Session {
  const v = useContext(Ctx);
  if (!v) throw new Error("useSession must be used inside <SessionProvider>");
  return v;
}

/** The session, or `null` outside a SessionProvider (a component that only wants to know whether an administrator is signed in). */
export function useOptionalSession(): Session | null {
  return useContext(Ctx);
}

export function SessionProvider({ children }: { children: ReactNode }) {
  const [status, setStatus] = useState<Session["status"]>("loading");
  const [admin, setAdmin] = useState<AdminInfo | null>(null);
  const [adminScope, setAdminScope] = useState<AdminScope | null>(null);
  const [mfaToken, setMfaToken] = useState<string | null>(null);
  const [mfaAs, setMfaAs] = useState<"admin" | "user">("admin");
  const [expiredNotice, setExpiredNotice] = useState(false);
  const [impersonation, setImpersonation] = useState<ImpersonationState | null>(null);
  const token = useRef<string | null>(null);
  const impRef = useRef<ImpersonationState | null>(null);
  const statusRef = useRef(status);
  useEffect(() => {
    statusRef.current = status;
  });

  const clearImpersonation = useCallback(() => {
    token.current = null;
    impRef.current = null;
    setImpersonation(null);
  }, []);

  const applyMe = useCallback((m: MeResponse | null): boolean => {
    // A realm administrator is a user or service account that holds identity:admin in its realm: `admin_scope` says so (FB-137).
    if (m && (m.subject.type === "admin" || m.admin_scope)) {
      setAdmin({ id: m.admin?.id ?? m.subject.id ?? "", email: m.admin?.email ?? "" });
      setAdminScope(m.admin_scope ?? "platform");
      setStatus("authenticated");
      return true;
    }
    setAdmin(null);
    setAdminScope(null);
    setStatus("anonymous");
    return false;
  }, []);

  const loadAdmin = useCallback(async (): Promise<boolean> => {
    try {
      return applyMe(await me());
    } catch {
      return applyMe(null);
    }
  }, [applyMe]);

  useEffect(() => {
    setTokenSource(() => token.current);
    setUnauthorizedHandler(() => {
      if (impRef.current) {
        // The impersonation token stopped working: fall back to the admin session.
        clearImpersonation();
        return;
      }
      if (statusRef.current === "authenticated") {
        setExpiredNotice(true);
        setAdmin(null);
        setAdminScope(null);
        setStatus("anonymous");
      }
    });
    let live = true;
    me().then(
      (m) => live && applyMe(m),
      () => live && applyMe(null),
    );
    return () => {
      live = false;
      setUnauthorizedHandler(null);
      setTokenSource(() => null);
    };
  }, [applyMe, clearImpersonation]);

  // Impersonation expires by itself.
  useEffect(() => {
    if (!impersonation) return;
    const ms = Math.max(0, new Date(impersonation.expiresAt).getTime() - Date.now());
    const t = setTimeout(clearImpersonation, ms);
    return () => clearTimeout(t);
  }, [impersonation, clearImpersonation]);

  const finish = useCallback(
    async (s: LoginSession, as: "admin" | "user" = "admin") => {
      setCsrfToken(s.csrf_token ?? null);
      setMfaToken(null);
      setExpiredNotice(false);
      if (!(await loadAdmin()))
        throw new ApiError({
          status: 403,
          code: "forbidden",
          message: as === "user" ? "This account is not a realm administrator" : "This account is not an administrator",
        });
    },
    [loadAdmin],
  );

  const signIn = useCallback(
    async (email: string, password: string, opts: SignInOptions = {}) => {
      const as = opts.as ?? "admin";
      const r = as === "user" ? await userLogin(email, password, opts.realm?.trim()) : await login(email, password);
      if (isMfaChallenge(r)) {
        setMfaToken(r.mfa_token);
        setMfaAs(as);
        return;
      }
      await finish(r, as);
    },
    [finish],
  );

  const submitMfa = useCallback(
    async (code: string) => {
      if (!mfaToken) throw new Error("No sign-in is waiting for a code");
      await finish(await mfaVerify(mfaToken, code), mfaAs);
    },
    [finish, mfaToken, mfaAs],
  );

  const signOut = useCallback(async () => {
    clearImpersonation();
    try {
      await logout();
    } catch {
      /* already gone */
    }
    setCsrfToken(null);
    setAdmin(null);
    setAdminScope(null);
    setMfaToken(null);
    setExpiredNotice(false);
    setStatus("anonymous");
  }, [clearImpersonation]);

  const startImpersonation = useCallback(async (principal: string, reason: string, ttlMinutes?: number) => {
    if (impRef.current) throw new Error("Already impersonating: end the current impersonation first");
    const g = await apiImpersonate(principal, reason, ttlMinutes);
    token.current = g.access_token;
    const st = { target: principal, reason: g.reason ?? reason, sessionId: g.session_id, expiresAt: g.expires_at };
    impRef.current = st;
    setImpersonation(st);
  }, []);

  const endImpersonation = useCallback(async () => {
    if (!impRef.current) return;
    try {
      await apiEndImpersonation();
    } catch {
      /* expired or revoked: ending locally is the same outcome */
    }
    clearImpersonation();
  }, [clearImpersonation]);

  const value = useMemo<Session>(
    () => ({
      status, admin, adminScope, mfaRequired: mfaToken !== null, expiredNotice, signIn, submitMfa, signOut, adoptSession: finish,
      impersonation, startImpersonation, endImpersonation,
    }),
    [status, admin, adminScope, mfaToken, expiredNotice, signIn, submitMfa, signOut, finish, impersonation, startImpersonation, endImpersonation],
  );
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}
