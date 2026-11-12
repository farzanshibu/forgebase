# MCP server

ForgeBase has a [Model Context Protocol](https://modelcontextprotocol.io) server, so an AI agent
(Claude Code, Cursor, or any MCP client) can operate and configure it: read and change data, files,
functions, jobs, users, API keys and server settings. The agent acts with the permissions of the API
key you give it, nothing more. Design: [ADR-0030](adr/0030-mcp-server.md).

## Turn it on

It is off by default. Turn it on in one of these ways, then restart the server:

- Dashboard: **System › Features and settings › MCP server**.
- `forgebase.toml`:

  ```toml
  [mcp]
  enabled = true
  ```

- Environment `FORGEBASE_MCP_ENABLED=true`, or `forgebase serve --mcp`.

The endpoint is `POST <server>/api/mcp` (for example `http://127.0.0.1:8090/api/mcp`).

## Give the agent a key

Agents authenticate with `Authorization: Bearer <key>`: a service account's API key (`fbk_…`) or a
personal access token (`fbp_…`). Create one in **Authentication › API keys & tokens**.

A key **with scopes** needs `mcp:*` (or `mcp:write`) to use the MCP server, plus the scopes of what
the agent may do. For example, an agent that may read and write records but nothing else:

```text
mcp:*  collections:read  collections:write
```

A key without scopes, or an administrator's personal access token, can do everything that
administrator can. Prefer a scoped service-account key, and revoke it when you are done.

## Connect a client

Claude Code:

```sh
claude mcp add --transport http forgebase http://127.0.0.1:8090/api/mcp \
  --header "Authorization: Bearer fbk_…"
```

Cursor (`.cursor/mcp.json`) and other clients that take a JSON config:

```json
{
  "mcpServers": {
    "forgebase": {
      "url": "http://127.0.0.1:8090/api/mcp",
      "headers": { "Authorization": "Bearer fbk_…" }
    }
  }
}
```

Keep the key out of files you commit: most clients read `${ENV_VAR}` in headers.

## What the agent gets

Named tools for common work:

| Area | Tools |
|---|---|
| Who and where | `whoami`, `server_info` |
| Settings | `get_features`, `update_config` (most changes apply after a restart) |
| Collections | `list_collections`, `get_collection`, `create_collection`, `update_collection`, `delete_collection` |
| Records | `list_records`, `get_record`, `create_record`, `update_record`, `delete_record` |
| SQL | `run_sql`, `list_tables` |
| Storage | `list_buckets`, `list_objects`, `read_object`, `write_object`, `delete_object` |
| Functions | `list_functions`, `get_function`, `deploy_function`, `invoke_function`, `function_logs`, `delete_function` |
| Jobs | `list_jobs`, `enqueue_job`, `get_job`, `cancel_job`, `retry_job` |
| Keys | `list_service_accounts`, `create_service_account`, `create_api_key`, `list_api_keys`, `revoke_api_key` |
| Audit | `audit_log` |

And three tools that reach every other operation of the REST API (users, realms, roles and rules,
buckets and signed URLs, queues, key-value, vector, graph, billing, backups, indexes, topology):
`list_operations` (by tag or search), `describe_operation` (parameters and schemas) and
`call_operation`. Tools of a module that is switched off are not listed.

## Safety

- **Same rules as the API.** Each tool call is an ordinary API request made with the agent's key:
  scopes, access rules, tenancy and rate limits apply. A `403` in a tool result means the key may
  not do it.
- **Confirmation.** Deleting, revoking, rotating, purging, flushing, restoring, promoting, rebuilding
  and rolling back need `confirm: true`; the tool descriptions tell the agent to ask you first.
- **Audit.** Every call that changes something is logged as `mcp.tool_call` in
  **Authentication › Audit** (tool, operation, status, request id).
- **Browsers.** A request with an `Origin` header must come from the server's own origin or one in
  `server.cors_allowed_origins`.
- **Secrets.** `create_api_key` returns the new secret once, in the tool result; settings that are
  secrets read as `null`.

## Protocol notes

Streamable HTTP with JSON responses, protocol revisions 2025-06-18, 2025-03-26 and 2024-11-05.
Stateless: no `Mcp-Session-Id`, `GET` and `DELETE` answer `405`, notifications `202`, batches `400`.
Messages up to 4 MiB; tool results cut long text and return binary bodies up to 1 MiB as base64.
OAuth sign-in for MCP clients is not supported yet: use an API key or personal access token.
