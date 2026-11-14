# Changelog

All notable changes to the ForgeBase Rust SDK. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

Brings the crate to the surface of the JavaScript SDK and adds the roadmap APIs of ForgeBase 0.3.x.

### Added

- **Modules** (each an accessor on `ForgeBase`): `access()` (permission checks, ACLs, overrides, collection and engine
  resource rules), `orgs()` (organizations, projects, members, invitations, teams, roles, role sets, realm bindings,
  suspensions), `identity()` (API keys, PATs, service accounts, OAuth clients, realms, login providers with the presets,
  SMS gateways, users, impersonation, audit, passkeys, phone, LDAP, account self-service), `credits()`, `vector()`,
  `graph()` (including openCypher with the `engine` field), `sql()`, `indexes()`, `documents()`, `customize()` (branding,
  domains, templates, mail provider and test send, agreements, hosted views), `system()` (info, capabilities, features,
  config, backups, database logins, extensions, topology), `alerts()` and `slos()`, `insights()`.
- **Billing admin** on `billing()` (customers, portal, subscriptions, orders, invoices, credit notes, catalog, providers,
  settings, dunning, stats, usage meters, maintenance) and **entitlements admin** on `entitlements()` (plans, features,
  add-ons, usage packs, quota policies, overrides, limits, trials, gauges, usage events and alerts).
- **Queue webhook tooling** (`queues().webhooks(sub)`): attempts, test send, redrive, retry, secret rotation and
  expire-previous, pause and resume, event catalog; the new topic and subscription fields (`description`, `example`,
  `push`, `pause_after_failures`, `ttl_ms`, grace-window and pause state); `patch_topic`, `patch_subscription`,
  `topic_metrics`.
- **Billing webhook inspector** (`billing().webhook_deliveries()`): list, get, replay, reverify.
- **Webhook verification** (`webhooks`): `verify`, `verify_any`, `construct_event`, `sign`; several `v1` values, constant-time
  comparison, tolerance, the test vectors of the JavaScript SDK.
- Streamed audit export (`insights().export_audit`) as CSV or JSONL.
- Examples `vector_graph_sql` and `webhook_verify`; mock tests per module and live tests for vector, graph, SQL,
  webhook tooling, alerts, insights, capabilities, identity, customization, billing admin, backups and indexes.

### Changed

- `openapi/openapi.json` and `src/generated.rs` regenerated from the 0.3.5 server plus the roadmap APIs.
  `scripts/generate_models.py` gained `TYPE_OVERRIDES` (widens `SloStatus.burn_rates`, whose values are `null`
  without traffic).
- New dependencies: `hmac` and `sha2` (RustCrypto, MIT OR Apache-2.0).
