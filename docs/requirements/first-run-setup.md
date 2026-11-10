# First-run setup: admin sign-up, feature map and configuration (FB-90, FB-91, FB-92)

Human request (2026-11-11): "on init or first server — option to create admin
sign up, followed by all feature map and config".

Today the first administrator can only be created with `forgebase admin create`
on the command line, and only billing can be switched off. This document is the
contract the three work packages build against in parallel. Change it only
through god.

## 1. Setup mode and the setup token (FB-90)

- The server is in **setup mode** while no administrator account exists.
- On `forgebase serve` in setup mode the server generates a random one-time
  **setup token** (at least 128 bits, URL-safe) and prints, to standard output
  only (never to the log file or log shipping), a line such as:
  `First run: create the administrator at http://127.0.0.1:8090/admin/setup?token=<token>`
  using the configured public URL when set, otherwise the bound address.
- The token lives in memory only, is compared in constant time, and is
  invalidated when the first administrator is created. A restart issues a new
  one. Attempts are rate limited per client address.
- `forgebase status` reports `setup: pending (no administrator)` or
  `setup: complete`.

### `forgebase init` (FB-90)

- On a terminal: after creating the project, ask
  `Create the administrator account now? [Y/n]`, then email, password and
  confirmation (hidden input), and create the admin the way
  `forgebase admin create` does.
- Non-interactive: `--admin-email <email>` with `--admin-password-stdin`.
- `--no-admin` skips the prompt; the output then says the first `serve`
  prints a setup link.

## 2. Setup API (FB-90)

All errors use the standard envelope
`{"error":{"code","message","request_id","details?"}}`.

| Method and path | Auth | Request | Response |
|---|---|---|---|
| `GET /api/setup/status` | none | | `200 {"setup_required": bool}` |
| `POST /api/setup/admin` | setup token | `{"token","email","password"}` | `201`, same body and session cookie as `POST /api/auth/admins/login` |

- `POST /api/setup/admin` errors: `403 invalid_setup_token`;
  `409 setup_complete` once any administrator exists; `400 validation_failed`
  (same password and email rules as `forgebase admin create`); `429` when rate
  limited.
- Both endpoints appear in `/api/openapi.json`.

## 3. Feature registry and configuration API (FB-91)

### Features

Every module is listed. **Always on:** `database`, `auth` (admins, users,
sessions). **Switchable** (each gets `[<id>] enabled`, default `true`):
`storage`, `functions`, `jobs`, `queues`, `realtime`, `vector`, `graph`,
`identity` (OIDC provider, realms), `access` (policies, organisations),
`resp` (Redis protocol listener), `billing` (already switchable). If a module
cannot be switched off safely, mark it `can_disable: false` and record why in
ADR-0026.

A disabled module's HTTP routes answer
`404 {"error":{"code":"feature_disabled","message":"<Name> is disabled on this server"}}`,
its background workers do not start, and its listener (for `resp`) is not
bound. Billing keeps its current behaviour and message, with code
`feature_disabled`.

### `GET /api/system/features` (administrator only)

```json
{
  "config_file": "/abs/path/forgebase.toml",
  "restart_pending": false,
  "features": [
    {
      "id": "storage",
      "name": "Object storage",
      "summary": "One-line description of what the module does.",
      "always_on": false,
      "can_disable": true,
      "enabled": true,
      "studio_path": "/storage",
      "docs": "docs/architecture.md#storage",
      "settings": [
        {
          "key": "storage.max_object_bytes",
          "label": "Largest object",
          "help": "One sentence.",
          "type": "int",
          "unit": "bytes",
          "options": null,
          "value": 104857600,
          "default": 104857600,
          "source": "default",
          "editable": true,
          "restart_required": false,
          "secret": false
        }
      ]
    }
  ]
}
```

- `type` is one of `bool`, `int`, `string`, `enum` (with `options`),
  `duration` (string such as `"30s"`), `list` (array of strings), `url`.
- `source` is one of `default`, `file`, `env`, `flag`. A setting whose source is
  `env` or `flag` has `editable: false`: the file cannot override it.
- Secrets (passwords, API keys, master key) are never listed with a value: list
  them with `secret: true`, `value: null`, `editable: false`, and say in `help`
  which environment variable sets them.
- Each feature lists its own `<id>.enabled` setting first when switchable, then
  the settings that already exist in `forgebase.example.toml` for that section.
  Server-wide settings (`server.*`, `log.*`, `database.*`) go under a `server`
  entry with `always_on: true`.

### `PATCH /api/system/config` (administrator only)

- Request: `{"set": {"<key>": <value>, ...}}`, where keys are the dotted keys
  from the registry.
- The server validates every key first, then writes them all to the config file
  in one atomic replace, keeping comments and layout (`toml_edit`). If the
  project has no config file, it creates one next to the data directory.
- Response `200`:
  `{"written": "/abs/path/forgebase.toml", "restart_required": bool, "applied": ["<key>", ...], "features": [ ... same as GET ... ]}`.
  `restart_pending` in later GETs stays true until the server restarts.
- Errors: `400 validation_failed` with `details: {"<key>": "reason"}`;
  `409 setting_overridden` for an `env` or `flag` source; `403` for a
  non-administrator.
- Applying without a restart is optional per setting. Report honestly with
  `restart_required`.

## 4. Dashboard (FB-92)

Route `/admin/setup`, all on PanelUI:

1. **Create administrator.** Shown when `GET /api/setup/status` says
   `setup_required`. Fields: email, password, confirm password, and the setup
   token (prefilled from `?token=`). On success the admin is signed in. While
   setup is required, the login page redirects here; after setup, `/setup`
   redirects to Overview.
2. **Feature map.** A card per feature: name, summary, an on/off switch where
   `can_disable`, an "always on" badge otherwise, and a link to its studio.
3. **Configure.** One section per enabled feature with its settings as typed
   PanelUI fields. Show a source badge (default, file, env, flag); env/flag
   settings are read-only with the reason; secrets show the help text only.
   Save sends one `PATCH /api/system/config` with the changed keys only and
   shows per-key validation errors inline.
4. **Done.** Says whether a restart is needed and how
   (`forgebase serve` again, or the service manager), and links to Overview.

Steps 2–4 are also reachable later from the System studio as
"Features & settings". Studios of disabled features are hidden from the nav
rail. A `feature_disabled` error renders as a friendly "disabled" panel with a
link to Features & settings.

## Shared rules

- Next free numbers: migration `0026`, ADR `0026` (FB-91 owns ADR-0026,
  "First-run setup and runtime feature configuration"; FB-90 adds its own
  section to it after merge if needed, or notes it in its done message).
- Do not add API keys, passwords or real data to examples or tests.
- Docs: `docs/deployment.md` gains a "First run" section (FB-90), and
  `docs/architecture.md` gains a feature-flags section (FB-91).
