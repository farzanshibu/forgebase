# ADR-0030: MCP server for AI agents

- Status: accepted
- Date: 2026-11-12
- Scope: `crates/forgebase-api/src/mcp/` (endpoint, tools, OpenAPI catalog),
  `crates/forgebase-core` (`[mcp]` switch), `crates/forgebase-cli` (`--mcp`).
- Builds on: ADR-0004 (credentials and scopes), ADR-0008 (policy), ADR-0026
  (feature switches), the OpenAPI document of `GET /api/openapi.json`.
- User guide: [docs/mcp.md](../mcp.md).

## Context

Developers drive their backend from AI agents (Claude Code, Cursor and other
Model Context Protocol clients). ForgeBase already has a complete REST API
(about 590 operations), scoped API keys and personal access tokens, a policy
engine and an audit log. The ask: "an MCP server for complete agent control
and config with proper auth".

## Decisions

### 1. An endpoint in the server, Streamable HTTP, stateless

`POST /api/mcp` takes one JSON-RPC 2.0 message per request and answers with
`application/json` (the Streamable HTTP transport, revisions 2025-06-18,
2025-03-26 and 2024-11-05). There are no sessions and no server-initiated
stream: `GET` and `DELETE` answer `405`. Notifications and client responses
answer `202`. Batches are refused (removed from the protocol in 2025-06-18).

The protocol is small (`initialize`, `ping`, `tools/list`, `tools/call`), so
it is written by hand on axum and serde_json: no new dependency.

The path is under `/api/` so the central scope check applies: a scoped
credential needs area `mcp` (`mcp:write` or `mcp:*`) to reach it. An admin
grants an agent key explicitly; existing keys do not become agent keys.

### 2. Authentication is the server's; no second authorization path

The auth layer has already turned the request's credential (`Authorization:
Bearer fbk_…`/`fbp_…`, `X-API-Key`, or a session cookie with `X-CSRF-Token`)
into the caller's context. Anonymous callers get `401` with
`WWW-Authenticate: Bearer`.

Every tool call becomes **one HTTP request sent through the same router**
(`tower::ServiceExt::oneshot` on the finished `Router`, every layer included)
carrying the caller's credential headers and `X-ForgeBase-*` headers. Scopes,
policy rules, tenancy, feature switches, rate limits and each handler's own
checks therefore apply exactly as they do over HTTP; an agent can never do more
than its credential could with `curl`. A tool cannot set credential headers.

Considered and rejected: calling services directly from tool code. It would
duplicate authorization for 590 operations and drift from the REST API.

### 3. Curated tools plus three generic ones

`tools/list` returns about 40 named tools for common work (whoami, server
info, features and `update_config`, collections, records, SQL, storage,
functions, jobs, service accounts and API keys, the audit log) and three
generic tools that reach everything else from the OpenAPI document:
`list_operations` (tags, or search), `describe_operation` (parameters and
schemas with references written out) and `call_operation`.

One tool per operation (590 tools) was rejected: agents choose worse from very
long lists. A curated tool is hidden when its module is switched off (its
operation is not in the document).

Not callable over MCP: `/api/mcp` itself, realtime and the KV subscribe stream
(streams), `/api/auth/oidc/*` (browser redirects), `/api/setup/*` (first run)
and the database node agent's routes.

### 4. Destructive calls need confirmation; writes are audited

`DELETE`, and `POST` to revoke, rotate, purge, flush, restore, promote,
rebuild, rollback or reset, need `confirm: true`; tools carry the MCP
`destructiveHint` and `readOnlyHint` annotations. Every call that is not a
`GET` writes `mcp.tool_call` to `_forgebase_auth_audit` (tool, method,
operation, path, status, request id), next to whatever the operation audits
itself.

### 5. Off by default; browsers kept out

`[mcp] enabled = false` by default (`FORGEBASE_MCP_ENABLED`, `--mcp`); the
feature gate answers `404 feature_disabled` until an administrator turns it
on. A request that carries `Origin` must come from the server's own origin or
one in `server.cors_allowed_origins` (DNS rebinding protection); agents send
no `Origin`.

### 6. Limits

MCP requests up to 4 MiB; a tool reads at most 8 MiB of an API answer, returns
binary bodies inline up to 1 MiB (base64) and cuts text at 200 000 bytes with
a note to narrow the request. Answers are `Cache-Control: no-store`.

## Consequences

- Agents get the whole API with the same rules as every other client; the
  admin controls their reach with ordinary scoped keys.
- OAuth 2.1 sign-in for MCP clients (protected resource metadata, IdP access
  tokens on the REST API, client registration) is not part of this decision;
  ADR-0019 lists IdP tokens on the API as a follow-up. Until then agents use
  API keys or personal access tokens.
- The endpoint holds a clone of the router it is mounted in (a reference
  cycle); a server builds one router for its lifetime, so nothing leaks in
  practice.
