# Database topology screen (FB-176)

System studio, tab **Database topology** (`/system/topology`). Spec: ADR-0029 §5. Every rule of
[dashboard-ux.md](../requirements/dashboard-ux.md) applies. Code: `dashboard/src/pages/system/topology/`, API client
`dashboard/src/api/topology.ts`.

## What is on the page

| Part | Component | Notes |
|---|---|---|
| Header, "Add replica" with "Add shard" in its menu | `PageHeader` | Hidden on SQLite |
| Summary strip | `SummaryStrip` | Backend and engine, servers healthy, highest lag, shards, tenants, last operation |
| One card per set | `SetCard` | Node rows with state chip, lag, lag sparkline, disk free, engine; row menu and card menu |
| Operations | `OperationsPanel` | Progress bar, stage, message, cancel where `cancellable`, link to `/jobs/j/<id>` |
| Tenants | `PlacementsPanel` | Filter by set and project, search, "Load more" (cursor), "Move to…" |
| Rebalance | `RebalancePanel` | "Plan rebalance" then "Run plan" confirmed with the plan id |
| One server | `EmptyState` | What a replica and a shard are, both actions, "Nothing changes until you add one." |
| SQLite | `PostgresOnly` | "Replicas and shards need PostgreSQL" with the copyable `forgebase db convert --to postgres` |
| Use from code | `UseFromCode` with a custom snippet | `consistency: "strong"` and `X-ForgeBase-Read-After` |

## Components you may reuse

- `ActionDialog` (`ui.tsx`): a typed-name confirmation like `ConfirmDialog`, plus the step-up sign-in. When the
  server answers `403 mfa_step_up_required` the dialog swaps its body for `StepUpForm` (email, password, then the
  authenticator code), adopts the new session and **runs the action again by itself**. Use it for any admin action
  the server protects with a second factor. Promote, node and shard removal, move and rebalance run are those today.
- `Chip` / `StateChip`: a coloured chip with a dot and words (colour is never the only signal). Hover says what the state means.
- `Sparkline`: one hour of lag as a small line with a text alternative (`role="img"`).
- `DialogShell`: a PanelUI dialog open while mounted.

## Live behaviour

- Topology: polled every 5 s while the tab is visible (a hidden tab keeps its last answer).
- Operations: polled every 2 s while one runs. Each running operation also opens the realtime channel
  `jobs:<id>` (server-sent events); any message refreshes the operation from the job. Without `EventSource` the poll alone does it.
- When the set of running operations changes the topology refreshes at once.

## Words

Sentence case; a server is a "server", a copy that follows the primary is a "replica", a separate database holding
some tenants is a "shard", a primary with its replicas is a "set". Terms with a "?" are in `glossary.ts`:
`replica`, `primary`, `lag`, `shard`, `promote`, `rebalance`, `freeze`, `read_after`, `topology_set`.

## Tenant moves and rebalance (FB-174)

The UI is built to the ADR shapes. Until FB-174 merges the server answers `501 not_implemented`; the screen shows
"Coming in this release" and changes nothing. Tests use `src/test/topologyMock.ts` (`implemented: true` fakes the finished routes).
