# terraform-provider-forgebase

Manage a [ForgeBase](../../README.md) server's configuration as code: collections and their
indexes and access rules, storage buckets and ACLs, queue topics and subscriptions, functions,
service accounts and API keys, OAuth clients, roles, organizations, projects, and entitlement
plans and features.

Built with [terraform-plugin-framework](https://developer.hashicorp.com/terraform/plugin/framework)
(protocol 6, Terraform 1.0 or newer). Every call goes through the server's public REST API; the
source of truth is its OpenAPI document (`sdk/js/openapi/openapi.json`, or `GET /api/openapi.json`
on a running server).

- Provider and resource reference: [`docs/`](docs/index.md)
- Examples: [`examples/`](examples) (a complete configuration is in [`examples/complete`](examples/complete/main.tf))

## Resources

| Resource | Manages | Update in place |
|---|---|---|
| `forgebase_collection` | A collection: typed fields, type options, defaults, `view_query`, tenancy | Yes (fields by id; rename with `renamed_from`) |
| `forgebase_index` | A manual index (B-tree, unique, partial, full-text, trigram) | Replaced on any change |
| `forgebase_collection_rules` | List / view / create / update / delete rules and per-field rules | Yes |
| `forgebase_storage_bucket` | A bucket: visibility, versioning, size limit, content types | Yes |
| `forgebase_storage_bucket_acl` | The full ACL of a bucket | Yes |
| `forgebase_queue_topic` | A topic: retention, limits, schema, labels, record source | Yes (source: replaced) |
| `forgebase_queue_subscription` | A pull or push (webhook / function) subscription | Yes (topic, ordering, filter: replaced) |
| `forgebase_function` | A JS/TS or WebAssembly function with env, secrets, capabilities, hooks, limits, kv rules | Yes (new version) |
| `forgebase_service_account` | A service account | Replaced on any change |
| `forgebase_api_key` | An API key of a service account | Replaced on any change |
| `forgebase_oauth_client` | An OAuth 2.0 / OIDC client app | Yes |
| `forgebase_role` | A realm or organization role | Yes |
| `forgebase_org` | An organization | Name only |
| `forgebase_project` | A project (and its environments, read-only) | Replaced on any change |
| `forgebase_feature`, `forgebase_plan` | Entitlement catalog | Yes |

Data sources: `forgebase_collection`, `forgebase_storage_bucket`, `forgebase_queue_topic`,
`forgebase_function`, `forgebase_service_account`, `forgebase_org`, `forgebase_project`,
`forgebase_plan`. Every resource supports `terraform import`.

## Use it

```hcl
terraform {
  required_providers {
    forgebase = { source = "forgebase/forgebase" }
  }
}

provider "forgebase" {
  # url   = "http://127.0.0.1:8090"   # or FORGEBASE_URL
  # token = "fbk_…"                   # or FORGEBASE_TOKEN
}

resource "forgebase_storage_bucket" "uploads" {
  name       = "uploads"
  visibility = "private"
}
```

Create the credential once, with a platform administrator:

```sh
forgebase admin create --email you@example.com --password-stdin
# sign in, then create a service account and an API key through the dashboard or:
#   POST /api/auth/service-accounts   {"name": "terraform", "role": "admin"}
#   POST /api/auth/service-accounts/{id}/keys   {"name": "terraform", "scopes": ["*"]}
export FORGEBASE_URL=http://127.0.0.1:8090
export FORGEBASE_TOKEN=fbk_…
```

The provider is not published to the Registry yet. To use a local build, put this in
`~/.terraformrc` and run `go build -o ~/bin/terraform-provider-forgebase .` (no `terraform init`
is needed with a dev override):

```hcl
provider_installation {
  dev_overrides { "forgebase/forgebase" = "/home/you/bin" }
  direct {}
}
```

## How it behaves

- **Drift.** Refresh re-reads every object. A 404 removes it from state, so the next plan
  re-creates it. Server-assigned values (ids, timestamps, versions, server defaults) are computed.
- **Replacement.** Attributes the API cannot change in place (names, a collection's `type`, a
  subscription's `topic`, …) force replacement; the plan says which.
- **Secrets.** Values the server returns once are kept in state as sensitive attributes:
  `forgebase_api_key.key`, `forgebase_oauth_client.client_secret`,
  `forgebase_queue_subscription.push_secret`. They cannot be recovered later, so imported objects
  have none. Function `secrets` are write-only on the server: the provider checks that each name
  exists but cannot see values. Protect the state file; it holds these values.
- **Functions.** `source_file` / `wasm_file` are hashed (`content_sha256`) at plan time, so a
  changed file redeploys; a version changed on the server shows as drift. A secrets- or
  kv-rules-only change does not deploy a new version.
- **Collection fields.** Declared in order; only the option keys you set are tracked, so server
  defaults are not drift. System fields of `auth` collections stay out of the way unless declared.
- **Webhook secret.** Changing a push webhook's endpoint makes the server issue a new signing
  secret; the provider captures it. Other changes leave the secret alone.

## Develop

```sh
go build ./...
go vet ./...
go test ./...                 # unit tests: protocol-level, against a mock server
go generate ./...             # regenerate docs/ from the schemas and examples/
```

Tests drive the provider through the Terraform plugin protocol (validate → plan → apply → read →
import), so defaults, plan modifiers, `RequiresReplace`, sensitive values and drift behave as in
Terraform. Three more layers need a running server, and are skipped otherwise:

```sh
# Protocol-level lifecycle tests against a real server (no Terraform CLI needed).
FORGEBASE_LIVE=1 FORGEBASE_URL=http://127.0.0.1:8090 FORGEBASE_TOKEN=fbk_… \
  go test ./internal/provider -run TestLive -v

# terraform-plugin-testing acceptance tests (needs the terraform CLI on PATH).
TF_ACC=1 FORGEBASE_URL=http://127.0.0.1:8090 FORGEBASE_TOKEN=fbk_… \
  go test ./internal/provider -run TestAcc -v
```

Run those against a throwaway server (`forgebase init && forgebase serve` in an empty
directory): they create and delete real objects. The credential must be a platform administrator.

The docs in `docs/` follow the layout `tfplugindocs` produces and are generated by
`internal/docgen` from the same schemas; a test fails when they are stale.

## Not covered

The API has no way to manage these, or they are not worth a Terraform resource yet:

- **Realms** and **environments**: the API creates them but can neither update nor delete them,
  so a resource would leak objects on destroy. Use the `forgebase_project` environments output.
- **Organization members, teams, role sets, ACL entries, overrides**: planned.
- **Service-account or OAuth-client secret rotation**: a one-off operation, not desired state.
  Replace the resource, or call the rotate endpoint.
- Billing, jobs, KV, vector collections, graph, backups, system configuration.
