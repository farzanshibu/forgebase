/** In-memory fake of `/api/v1/system/backups` (ADR-0024). Every reply is contract-checked against the OpenAPI fixture (`mock.problems` must stay empty). */
import type { BackupManifest, BackupSnapshot, Retention, VerifyReport } from "../api";
import { hasResponse, matchRoute, responseSchemaName, validateBody } from "./contract";
import { envelope, type MockApi, type MockReply, type MockRequest } from "./mockApi";

export const snapshot = (id: string, size: number, extra: Partial<BackupManifest> = {}): BackupSnapshot => ({
  id,
  manifest: {
    format: 1, id, kind: "snapshot", created: `${id.slice(0, 10)}T${id.slice(11, 13)}:00:00Z`, forgebase_version: "0.1.0", schema_version: 24, includes_indexes: true,
    master_key: { included: false, fingerprint: "0123456789abcdef" },
    files: [{ path: "forgebase.db", size, sha256: "a".repeat(64), kind: "database" }, { path: "storage/ab/blob", size: 10, sha256: "b".repeat(64), kind: "storage" }],
    ...extra,
  },
});

export interface BackupState {
  items: BackupSnapshot[];
  /** Snapshot ids whose verification fails. */
  broken: Record<string, string[]>;
  created: { label?: string; include_indexes?: boolean; retention?: Retention }[];
  pruned: Retention[];
  /** Make every call answer 403 (a non-admin). */
  forbidden: boolean;
}

export function installBackups(mock: MockApi): BackupState {
  const st: BackupState = {
    items: [snapshot("2026-11-09T10-00-00Z", 2048), snapshot("2026-11-08T10-00-00Z", 5 * 1024 * 1024, { label: "before upgrade" })],
    broken: {}, created: [], pruned: [], forbidden: false,
  };
  const route = (method: string, template: string, h: (r: MockRequest) => MockReply) => mock.on(method, template, (req) => {
    const out = st.forbidden ? envelope(403, "forbidden", "backups are for administrators") : h(req);
    const spec = matchRoute(method, req.path);
    if (!spec) mock.problems.push(`not in openapi.json: ${method} ${req.path}`);
    else if (out.status < 400) {
      if (!hasResponse(spec.op, out.status)) mock.problems.push(`${method} ${template}: status ${out.status} not documented`);
      const name = responseSchemaName(spec.op, out.status);
      if (name && out.body !== undefined) for (const p of validateBody(name, out.body)) mock.problems.push(`${method} ${template} -> ${name}: ${p}`);
    }
    return out;
  });
  const B = "/api/v1/system/backups";
  route("GET", B, () => ({ status: 200, body: { items: st.items } }));
  route("POST", B, ({ body }) => {
    const b = (body ?? {}) as BackupState["created"][number];
    st.created.push(b);
    const s = snapshot(`2026-11-10T0${st.created.length}-00-00Z`, 4096, b.label ? { label: b.label } : {});
    st.items.unshift(s);
    return { status: 201, body: s.manifest };
  });
  route("POST", `${B}/prune`, ({ body }) => {
    const r = body as Retention;
    st.pruned.push(r);
    const doomed = st.items.slice(r.keep_last ?? st.items.length).map((s) => s.id);
    st.items = st.items.filter((s) => !doomed.includes(s.id));
    return { status: 200, body: { deleted: doomed } };
  });
  route("GET", `${B}/{id}`, ({ params }) => {
    const s = st.items.find((x) => x.id === params.id);
    return s ? { status: 200, body: s } : envelope(404, "not_found", "no such snapshot");
  });
  route("DELETE", `${B}/{id}`, ({ params }) => {
    if (!st.items.some((x) => x.id === params.id)) return envelope(404, "not_found", "no such snapshot");
    st.items = st.items.filter((x) => x.id !== params.id);
    return { status: 204 };
  });
  route("POST", `${B}/{id}/verify`, ({ params }) => {
    const s = st.items.find((x) => x.id === params.id);
    if (!s) return envelope(404, "not_found", "no such snapshot");
    const problems = st.broken[s.id] ?? [];
    const r: VerifyReport = { ok: problems.length === 0, manifest: s.manifest, files_checked: s.manifest.files.length, bytes_checked: 2058, problems, warnings: problems.length ? [] : ["auth/master.key is not in this backup"] };
    return { status: 200, body: r };
  });
  return st;
}
