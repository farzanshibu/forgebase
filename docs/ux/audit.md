# Dashboard UX audit (FB-97, card MDF-80)

Audited 2026-11-11 against [docs/requirements/dashboard-ux.md](../requirements/dashboard-ux.md)
(the ten-minute first journey and rules 1 to 9). Build: ForgeBase 0.2.0 release binary with its
embedded dashboard (main at 40890aa), plus the unmerged branches `agent/worker-fb92` (first-run
setup, Features and settings) and `agent/worker-fb94` (function code editor), read from git
without merging. Work packages: [work-packages.md](work-packages.md). Browser walkthrough:
[walkthrough.md](walkthrough.md) and `dashboard/e2e/journey.spec.ts`.

## Summary

- **The journey cannot be finished in the browser today.** Step 1 (create the administrator)
  needs the command line, step 3 (copy a snippet) has no screen at all, and step 5 (write a
  function) needs a WebAssembly toolchain the developer does not have. Steps 2, 4 and 6 work,
  but each needs guessing: a record is typed as raw JSON, the bucket form is at the bottom of
  the page, and the app's API key hides under Service accounts.
- **The studios are capable, but each one does things its own way.** There are three `Empty`
  components and about 45 bare "No X yet." lines. There are 48 inline "Saved." messages and
  toasts in only two places. Create forms appear in four different layouts. 51 deletes use the
  same generic "This cannot be undone." dialog, and none asks you to type the name.
- **Navigation and orientation are missing.** The nav has six groups instead of the three in the
  contract. There is no command palette and there are no breadcrumbs. No page links to its
  docs, and studio names differ between the nav, the top bar and the page title.
- **Accessibility is in good shape.** axe-core 4.14 (WCAG 2.0/2.1 A and AA) found no serious or
  critical issue on the 14 studio main pages and 22 tab and detail pages, in light and in dark,
  empty and with data. At 768 px the rail becomes a drawer and pages stay readable. At 1024 px nothing
  overflows except the 13 Billing tabs (A-23).
- **Branch fb92 closes step 1, and branch fb94 closes most of step 5.** Both still miss parts of
  rules 1, 4, 6 and 8 (listed under A-01 and A-05).

Screenshots are outside the repo in `/Volumes/Vinu1TBSSD/munder-difflin/.cache/ux-audit/`.
They are named `<state>-<route>-<theme>[-w<width>].png`, where state is `anon`, `empty` or
`data`, and `journey-<step>-<theme>.png` comes from the e2e script. The axe reports are
`report-*.json`.

## How findings are ranked

- **P1**: blocks the journey or confuses.
- **P2**: slows the developer down.
- **P3**: polish.

Each finding gives the studio and screen, what a new developer sees, the rule it breaks, the fix,
and the files involved. The package letter in brackets, such as **[a]**, points to
[work-packages.md](work-packages.md).

---

## P1: blocks the journey or confuses

### A-01: No administrator means a dead end at sign-in (journey step 1) [e]
- **Where:** sign-in page on a fresh server (`anon-overview-light.png`, `anon-overview-dark.png`).
- **What a new developer sees:** "Sign in to manage this server." with email and password
  fields. Nothing says that no administrator exists yet or how to create one. The only way is
  `forgebase admin create` on the command line.
- **Rules:** journey step 1, rule 2 and rule 4.
- **Fix:** merge `agent/worker-fb92`. It sends `/admin/` to `/admin/setup` while setup is
  pending, and `forgebase serve` prints the setup link. Then close what that branch still misses:
  - Show the password rule as help text under Password, and check that Confirm matches when the
    field loses focus, not only on submit (`pages/setup/SetupPage.tsx:79,117-126`).
  - Field errors sit inside the `<label>`, so they become part of the input's name. Wire them
    with `aria-describedby` instead (`SetupPage.tsx:112-126`, `SettingField.tsx:49,60,92`). Use
    the shared `FormField` from A-04.
  - Show each feature's `docs` link (already in `api/setup.ts:36`) on its card
    (`FeaturesFlow.tsx:159-185`), and add a tooltip that explains "always on" and the source
    badges "default", "file", "env" and "flag".
  - In System, Features and settings: changing one setting takes three wizard screens. Use one
    form with a sticky Save button and a toast there. Keep the steps for first-run setup only.
  - Bug: the "Open X studio" link shows for an unsaved switch (`FeaturesFlow.tsx:179`). Use
    `f.enabled`.
  - On a server where setup was done from the command line, add one line to `LoginPage.tsx`:
    "Forgot which account? Run `forgebase admin create --email you@example.com` on the server."
- **Files:** `dashboard/src/pages/setup/*` (fb92), `dashboard/src/pages/LoginPage.tsx`.

### A-02: Overview is a metrics wall, not a starting point (journey, "Overview is the starting point") [b]
- **Where:** Overview (`empty-overview-light.png`, `data-overview-dark.png`).
- **What a new developer sees:** ten metric cards: database size, "WAL", p50 and p95, "RSS",
  and four cards that read "not reported by this server". There is no next step, no checklist
  and no server URL to connect to. The nav calls this page "Dashboard".
- **Rules:** the "Overview is the starting point" section, and rule 1.
- **Fix:**
  - **Getting started** at the top: five rows with a done or not-done state read from the
    server, each with a link:
    - administrator exists (always done once signed in);
    - a collection that is not a system collection exists (`listCollections`), linking to
      `/database/new`;
    - an API key exists (service-account keys or personal tokens), linking to the key wizard (A-08);
    - a file is uploaded (storage totals `objects > 0`), linking to `/storage`;
    - a function is deployed (`listFunctions`), linking to `/functions/new` once fb94 lands.
  - The checklist can be collapsed. Remember that per browser.
  - **Connect your app** beside it:
    - the server URL from `window.location.origin`, with a copy button;
    - "Create an API key" linking to the wizard (A-08);
    - a `UseFromCode` block (A-03) with the install command and a first query against the
      newest collection.
  - Move health, version and activity below under a "Server health" heading.
- **Files:** `dashboard/src/pages/OverviewPage.tsx`, new `dashboard/src/pages/overview/GettingStarted.tsx`
  and `ConnectApp.tsx`.

### A-03: No "Use from code" anywhere (journey step 3, rule 3) [a, then b to e]
- **Where:** every resource screen: collection, record, bucket, function, queue topic, job kind,
  key-value namespace, vector collection and graph.
- **What a new developer sees:** no snippet, no server URL and no SDK name. The only way to learn
  the API is `docs/sdk.md` in the repository.
- **Rule:** rule 3.
- **Fix:**
  - **Component (package a):** `components/UseFromCode.tsx`, a PanelUI `Tabs` with JavaScript,
    Rust and curl tabs. Each code block has a copy button. The server URL comes from
    `location.origin`, the key is always `<YOUR_API_KEY>`, and the resource name is filled in.
  - **Generators (package a):** `src/snippets.ts`, one function per resource that returns
    `{ js, rust, curl }`. JS uses `new ForgeBase(url)` and `fb.collection(name).list()` from
    `@forgebase/sdk`. Rust uses `forgebase-sdk`. Where the JS SDK has no wrapper (storage,
    functions, key-value, vector, graph), use `fb.request(path)`.
  - **Placement (packages b to e):** in each detail page's header, as a "Use from code" button
    that opens the panel in a side sheet.
  - **Unit test:** a snippet never contains a real key and always contains the name and origin.
- **Files:** new `dashboard/src/components/UseFromCode.tsx` and `dashboard/src/snippets.ts`. Used
  in `pages/database/CollectionView.tsx`, `pages/storage/BucketView.tsx`,
  `pages/functions/FunctionView.tsx`, `pages/queues/TopicDetail.tsx`, `pages/redis/RedisPage.tsx`,
  `pages/vector/VectorCollectionView.tsx`, `pages/graph/GraphOverview.tsx` and
  `pages/jobs/JobList.tsx`.

### A-04: Errors show a code, not the fix (rule 4) [a]
- **Where:** the shared `ErrorView` (`dashboard/src/components/ui.tsx:36-80`), used on every screen.
- **What a new developer sees:**
  - A title such as `invalid_name: invalid name "": use letters, digits and underscores…`, with
    the machine code first in monospace.
  - The request id as plain text with no copy button.
  - No "what to do next".
  - Any 401 or 403 is titled "Sign-in required", so a missing permission reads as a lost session.
  - Field errors are a list under the form, not next to their fields (SchemaDesigner,
    RecordEditor and every create form).
  - Server parser messages pass through word for word, for example "Failed to deserialize the
    JSON body into the target type: labels: unknown field `labels`…" from graph and vector.
- **Rule:** rule 4.
- **Fix:**
  - **`components/ErrorPanel.tsx`:**
    - The title is a plain sentence from `src/errors.ts`, a map from error code to
      `{ title, next }`. For example, `invalid_name` becomes "That name is not allowed", with the
      next step "Use letters, digits and underscores, starting with a letter."
    - It falls back to the server message.
    - A "Copy request id" button.
    - A "Details" disclosure with the raw JSON.
    - 401 says "Your session ended. Sign in again." 403 says "Your account lacks permission
      `<scope>`. Ask an administrator."
  - **`components/FormField.tsx`:** label, help text, and an error slot wired with
    `aria-describedby`. Forms map `error.fields[name]` into that slot.
  - Keep `ErrorView` as a thin alias during the move.
- **Files:** `dashboard/src/components/ui.tsx`, new `components/ErrorPanel.tsx`,
  `components/FormField.tsx` and `src/errors.ts`.

### A-05: Writing a function needs a WebAssembly toolchain (journey step 5) [d]
- **Where:** Functions, then "Deploy function" (`empty-functions-light.png`,
  `journey-05-functions-*.png`).
- **What a new developer sees:**
  - A form that asks for a `.wasm` file, plus capabilities, fetch hosts, environment and record
    hooks.
  - No template, no example module and no "how do I build one". The repo has none either.
  - The studio summary says "WASM functions and schedules", but there are no schedules here.
- **Rules:** journey step 5, rules 2 and 8.
- **Fix:** merge `agent/worker-fb94` (New function, Monaco editor, templates, Test tab). Then
  close what it still misses:
  - No template reads a collection (`templates.ts:81-87`), and the webhook template hard-codes a
    collection named `orders` (`:84`). Add a "Read a collection" HTTP template with a collection
    picker and `db.read` switched on.
  - After Deploy, nothing confirms it and nothing leads to testing (`NewFunction.tsx:89`,
    `EditorTab.tsx:27`). Show a toast "Deployed v1" and open `…/test`.
  - The Test panel shows the request id as plain text with no copy button (`TestPanel.tsx:89`).
    Without an `x-request-id` header it shows the newest execution's logs, which may belong to
    another call (`TestPanel.tsx:49-51`). Send a request id and filter the logs by it.
  - The list has both "New function" and "Deploy function". Rename the second "Upload .wasm"
    and make it a secondary action (`FunctionList.tsx:31-32`).
  - Explain "permission", "authenticated" and "public" (HTTP access), `kv`, `env`, `fetch` and
    `service_role` with keyboard-reachable help tips. Today the "admin only" note is a
    mouse-only tooltip (`NewFunction.tsx:66-69`, `CapabilitySwitches.tsx:14-16`).
- **Files:** `dashboard/src/pages/functions/*`.

### A-06: No command palette or breadcrumbs, and six nav groups instead of three (rule 7) [a]
- **Where:** the app shell (`dashboard/src/components/Layout.tsx:49-56`).
- **What a new developer sees:**
  - 14 items under Overview, Data, Identity and access, Compute, Business and Operations.
  - At 1280×860 the last item, "System Studio", is cut off and needs scrolling.
  - `Ctrl/Cmd+K` does nothing.
  - Detail pages use hand-made back links: "← All topics" (queues), "All buckets" (storage) and
    "Back to data" (record), or none at all (functions, vector).
- **Rule:** rule 7.
- **Fix:**
  - **Groups**, defined once in `studios.ts` (a `group` field):
    - Build: Database, Storage, Functions, Key-value, Vector, Graph, Queues, Jobs.
    - Users and access: Authentication, Access, Identity, Billing.
    - Operate: System. Features and settings joins once fb92 lands.
  - Overview stays on top, ungrouped.
  - **`components/CommandPalette.tsx`:** PanelUI `Command` in a `Dialog`, opened by
    `Ctrl/Cmd+K` and by a "Search" button in the top bar. Its sources:
    - every studio and tab from `studios.ts`;
    - actions such as "New collection", "New bucket", "New function" and "Create API key";
    - resources loaded lazily on open: collections, buckets, functions, topics and vector
      collections.
  - **`components/Breadcrumbs.tsx`:** fed from the route. It replaces the top bar's
    label-and-summary pair (A-10), which today repeats the page title.
- **Files:** `dashboard/src/components/Layout.tsx`, `dashboard/src/studios.ts`, new
  `components/CommandPalette.tsx` and `components/Breadcrumbs.tsx`.

### A-07: Adding a record means typing JSON by hand (journey step 2) [c]
- **Where:** Database, then the collection, then "Add record" (`journey-02b-records-*.png`;
  `dashboard/src/pages/database/RecordEditor.tsx:63-70`).
- **What a new developer sees:**
  - A blank monospace textarea holding `{ }`. Field names, types and which fields are required
    are not shown; you must remember them from the schema tab.
  - A bool must be typed as `false`, and a date in the server's format.
  - A JSON typo gives "Invalid JSON: Unexpected token…" under the box.
- **Rules:** journey step 2, rules 4 and 6.
- **Fix:**
  - **`pages/database/RecordForm.tsx`**, generated from `collection.fields`. One control per
    type:
    - text and email: `Input`;
    - number: `Input type=number`;
    - bool: `Switch`;
    - date: date input;
    - select: `Select` of the field's values;
    - json and array: a small JSON editor;
    - relation: a record picker;
    - file: an upload.
  - Required fields are marked, and field errors appear under their field (A-04).
  - An "Edit as JSON" toggle keeps today's editor for power users.
  - Open the form in a side sheet from the grid instead of a separate route (A-11).
- **Files:** `dashboard/src/pages/database/RecordEditor.tsx`, `CollectionGrid.tsx`, new `RecordForm.tsx`.

### A-08: The API key for an app is hard to find and hard to scope (journey step 3) [e]
- **Where:** Authentication, then "API keys & tokens" (`journey-03-token-*.png`), and
  Authentication, then "Service accounts".
- **What a new developer sees:**
  - "New personal access token (for you)", with free-text scopes and the placeholder
    `collections:read`.
  - A key for an app needs a service account first, on another tab, and nothing says so.
  - The scope grammar (`collections:read:<name>`) is explained only by the server's 403 after the
    fact: "insufficient_scope: this request needs scope collections:read:ux_notes…". The
    walkthrough first tried `records:read`, which looks reasonable and fails.
- **Rules:** journey step 3, rules 6 and 8.
- **Fix:**
  - **A "Create API key" dialog** that every "Create an API key" link opens (Overview, palette
    and Use from code). Its steps:
    1. Pick a service account, or create one named after the app.
    2. Pick scopes with checkboxes grouped by resource, for example "Read records: all
       collections / ux_notes", with the scope string shown under each.
    3. Choose an expiry.
  - It shows the key once (the existing `SecretModal`) next to a curl snippet that uses it.
  - Rename the tab "API keys". Keep personal tokens as a secondary section titled "Personal
    tokens (for scripts you run yourself)".
- **Files:** `dashboard/src/pages/auth/ApiKeys.tsx`, `ServiceAccounts.tsx`, `common.tsx`, new
  `pages/auth/CreateKeyDialog.tsx` and `pages/auth/scopes.ts`.

### A-09: Empty states do not teach (rule 2) [a, then c to e]
- **Where:** about 45 places, for example "No functions yet." (`FunctionList.tsx:35`), "No
  vector collections yet." (`VectorCollections.tsx:53`), "No users match." (`auth/Users.tsx:52`),
  "No organizations yet." and "No service accounts yet." There are three separate `Empty`
  components (`pages/access/shared.tsx`, `pages/identity/shared.tsx`,
  `pages/billing/shared.tsx`) and many plain `<p>` lines.
- **What a new developer sees:** one grey line. It does not say what the thing is or why you
  would want one, and the create action is often somewhere else on the page.
- **Rule:** rule 2.
- **Fix:** `components/EmptyState.tsx`, with an icon, a title, one sentence on why, a primary
  action, an optional "Try a sample" action and a docs link. Replace every bare line and the
  three `Empty` components. Starter copy:
  - Functions: "Functions run your code next to your data. Start from a template that reads a
    collection." [New function]
  - Buckets: "Buckets hold files your app uploads, like avatars or exports." [New bucket]
  - Vector: "Vector collections store embeddings for similarity search." [New collection] [Load sample]
- **Files:** new `dashboard/src/components/EmptyState.tsx`, then every studio list page.

### A-10: Page headers lack a description, a docs link and a consistent primary action (rule 1) [a, then b to e]
- **Where:** every studio (`empty-*-light.png`).
- **What a new developer sees:**
  - The top bar shows the studio name and summary, then the page repeats the name as `h1`.
  - Most pages have no sentence saying what they are for. Where one exists (Overview, Database,
    Functions, Vector, System), it sits in the body, not in the header.
  - No page links to the docs.
  - The primary action moves around:
    - "New collection" is a link styled as a button in the body (`CollectionBrowser.tsx:13`);
    - "Create bucket" is a form at the bottom of Storage (`BucketList.tsx:81-87`);
    - "New topic" and "New organization" are inline forms above their lists;
    - "Deploy function" toggles a form open.
- **Rule:** rule 1.
- **Fix:**
  - **`components/PageHeader.tsx`** with `title`, `description`, `docsHref` and a `primary`
    action at the top right, using `Button` and an optional secondary menu. Docs links come from
    `studios.ts` (`docs` field) and point at the repo docs (see question Q2 in work-packages.md).
  - The top bar keeps breadcrumbs, search and the account. It drops the duplicated label and
    summary.
- **Files:** new `dashboard/src/components/PageHeader.tsx`, `components/Layout.tsx` and
  `studios.ts`. Each studio's `*Page.tsx` adopts it.

---

## P2: slows the developer down

### A-11: Four ways to create a thing (rule 5) [a, then c to e]
- **What a new developer sees:**
  - a separate route (`/database/new`);
  - an inline form at the bottom (storage) or the top (queues, access, auth service accounts);
  - a toggled form (functions);
  - a modal (secrets).
- **Fix:** the `PageHeader` primary action opens a PanelUI `Sheet` holding the create form.
  Success closes the sheet, shows a toast and opens the new resource. Detail drawers use the
  same `Sheet`. Pattern component: `components/CreateSheet.tsx`.

### A-12: Feedback is slow and inconsistent (rule 6) [a, then c to e]
- **Facts:**
  - There are no skeletons; `Loading…` with a spinner appears 93 times.
  - Toasts are used in two places only (`pages/auth/common.tsx`, `pages/storage/SignedUrls.tsx`).
  - 48 inline `role="status"` "Saved." lines, such as `SchemaDesigner.tsx:126` and
    `RecordEditor.tsx:71`.
  - There is no validation before submit. For example, a collection name is checked only by the
    server: `invalid name "": use letters, digits and underscores…`.
- **Fix:**
  - `components/Skeleton.tsx` (table rows and cards) as the `Loading` replacement for lists.
  - `notify.saved(what)` and `notify.failed(error)` in `src/toaster.ts`, called on every save.
  - Validators in `src/validate.ts` that mirror the server's name rule (letters, digits and
    underscores, starting with a letter, at most 63 characters) and run as you type.

### A-13: Deletes never ask for the name, and the bucket delete option is global (rule 5) [a, then c to e]
- **Facts:**
  - `ConfirmButton` (`ui.tsx:18-33`) is used 51 times. Its body always reads "This cannot be
    undone."
  - Deleting a collection, a bucket with its objects, a function, a topic, or flushing a
    key-value namespace never asks you to type the name.
  - Storage has one "Delete with their objects" checkbox in the panel header that changes what
    every row's Delete does (`BucketList.tsx:26`).
- **Fix:**
  - `components/ConfirmDialog.tsx` with `title`, `consequence` (what is lost, in plain words),
    and a `confirmText` that, when set, must be typed exactly.
  - Use `confirmText={name}` for collection, bucket, function, topic, namespace flush, realm and
    organization.
  - Move "also delete its objects" into the bucket's own dialog.

### A-14: The same thing has different names (rule 8) [a]
- **Facts:**
  - The nav says "Dashboard"; the contract and journey say "Overview".
  - The top bar says "Queues Studio", the page says "Queues". Likewise "System Studio" and
    "System".
  - "Identity & Tenant Studio" (nav) against "Identity and Tenant Studio" (h1).
  - The key-value engine is "Redis Studio" in the nav, "Key-value store" on Overview, and
    "key-value namespace" in the contract.
  - Jobs' summary says "Queues, jobs, schedules" while there is a separate Queues studio.
  - Functions' summary promises "schedules" that live in Jobs.
- **Fix:** one record per studio in `studios.ts` (`label`, `title`, `summary`, `group`, `docs`),
  read by the nav, the top bar, `PageHeader` and the palette. Drop the "Studio" suffix. Rename
  per question Q1 in work-packages.md.

### A-15: Jargon without help (rule 8) [a for the component, then c to e]
- **Examples:**
  - Database: type `base`, `auth` or `view`; schema mode `strict` or `flexible`; "Options (JSON)"
    with `{"max_length":80}`.
  - Functions: "principal service:function.x", "fuel", "HTTP access permission", capabilities.
  - Storage: "Run reconcile", "Access (ACL)", "Versioning".
  - Queues: "Ack deadline", "Dead-letter topic", "Ordering key", "Dedup id".
  - Vector: "Exact (brute force)", "Hybrid", "k".
  - Billing: "Dunning".
  - Overview: "scrapes", "WAL", "RSS", "p50" and "p95".
- **Fix:** `components/HelpTip.tsx`, a small "?" button reachable by keyboard. Its text is linked
  with `aria-describedby` and comes from `src/glossary.ts`. Each studio wraps its terms. Replace
  "Options (JSON)" with typed option inputs per field type (`SchemaDesigner.tsx:115`).

### A-16: Overview cards say "not reported" and "collecting…" at once, forever [b]
- **Where:** `pages/OverviewPage.tsx:16,85` (`empty-overview-light.png`): Requests per second,
  Error rate, Realtime and CPU.
- **Fix:** when a metric is absent from two polls in a row, drop the card, or show one muted line
  "This server does not report X" with a docs link. Show "collecting…" only while a reported
  metric has fewer than two samples.

### A-17: Stale "coming later" copy contradicts the screens [e]
- **Where:** System health (`pages/SystemPage.tsx:167`) says "Server and error logs, audit logs,
  migrations, backups, restore, configuration and system metrics arrive in phase 9." A Backups
  tab sits next to it, and the audit log is in Authentication. The `phase` and `features` fields
  in `studios.ts` and `components/StudioPlaceholder.tsx` are left over from the roadmap.
- **Fix:** replace the paragraph with links to Backups, Authentication's Audit log and (after
  fb92) Features and settings. Remove `StudioPlaceholder`, `phase` and `features` (package a
  owns `studios.ts`).

### A-18: The collection grid hides types and filters, and its state is lost on reload [c]
- **Where:** `pages/database/CollectionGrid.tsx` (`data-database_c_ux_notes_light_35qa7-light.png`).
- **Facts:**
  - Column headers run the name and type together ("title text", "done bool").
  - The filter syntax `price > 10 && name ~ "pen"` has no help.
  - "JSON" is the only way to open a record.
  - The filter and page are not in the URL, so a deep link or reload loses them.
- **Fix:**
  - Show the type as a muted badge after the name.
  - Add a filter `HelpTip` listing the operators.
  - Make the row click open the record sheet (A-07).
  - Keep `?filter=&page=` in `useSearchParams`.

### A-19: Three kinds of pagination (rule 5) [a, then c to e]
- **Facts:**
  - Previous and Next with a page count in Database (`CollectionGrid.tsx:191-197`).
  - "Load more" in Storage, function Logs and Graph (`usePaged.ts`).
  - Only "N keys shown" in Redis.
- **Fix:** `components/Pager.tsx` and a `useCursorList` hook. Use "Load more" for streams (logs,
  objects, messages), and Previous and Next with a count for tables.

### A-20: Function logs are hard to tie to an error [d]
- **Where:** `pages/functions/LogsTab.tsx:36-56` (`journey-06-logs-*.png`).
- **Facts:**
  - Each row reads "ok v1 http 0 ms 26422 fuel <time> <request id>".
  - The request id cannot be copied or searched.
  - "fuel" is unexplained.
  - An error's message is one red line with no next step.
  - There is no link from an `ErrorPanel` request id to the matching log.
- **Fix:**
  - Add a filter by request id, with the URL `?request=`.
  - Rows use `ErrorPanel` for errors.
  - Add copy buttons and a `HelpTip` for fuel ("work units the function used; the limit stops
    runaway code").
  - `ErrorPanel` links "Find in logs" when the error came from a function.

### A-21: A silent 400 on every Overview and Storage load [c]
- **Where:** `dashboard/src/api/storage.ts:151` calls `GET /api/v1/entitlements` with no
  subject. The server answers `400 subject_required` on each poll (browser console). The Storage
  card then says "No storage quota is reported for this account", which is misleading for an
  administrator.
- **Fix:** pass the subject the quota belongs to, or skip the call for administrators. Show
  "No quota set" with a link to where quotas are configured.

### A-22: Two studios have a tab row with one tab [c, d]
- **Where:** `pages/functions/FunctionsPage.tsx:10` ("Functions") and
  `pages/vector/VectorPage.tsx:10` ("Collections").
- **Fix:** remove the row until a second tab exists.

---

## P3: polish

### A-23: Billing's 13 tabs overflow [e]
- **Where:** `pages/billing/BillingPage.tsx:22-34` (`data-billing-dark-w768.png`). Seven tabs are
  visible at 768 px and the rest scroll.
- **Fix:** four groups: Catalog (Catalog, Coupons, Stores); Customers (Customers, Subscriptions,
  Orders, Usage); Money (Revenue, Invoices, Payments, Credits, Dunning); Settings. Use a
  secondary tab row or the rail's sub-items.

### A-24: Native inputs mixed with PanelUI controls (rules 5 and 9) [a for controls, then c to e]
- **Where:** 21 native `<input type="checkbox">` and other raw inputs, for example
  `SchemaDesigner.tsx:93,112-113`, `BucketList.tsx:26,85`, `DeployForm.tsx:89` and
  `LogsTab.tsx:28`.
- **Fix:** PanelUI `Checkbox` and `Switch`, exported through `components/controls.tsx` with the
  same value-in, value-out API as `Sel`.

### A-25: Theme and accessibility (passes; keep it that way) [a]
- **Fact:** axe found no serious or critical issue in either theme. Focus rings are visible and
  the contrast tests in `theme/contrast.test.ts` hold.
- **Keep it:** the e2e accessibility pass in `journey.spec.ts` should run in CI. Every new
  shared component gets a unit test with `jest-axe` or an equivalent role and label assertions.

---

## Branch notes (read from git, not merged)

| Branch | Delivers | Still open (finding) | Touches files other work also touches |
|---|---|---|---|
| `agent/worker-fb92` | `/admin/setup` wizard; Features and settings (System) | A-01 sub-items; its own headers, field errors, "server rejected" list and FeatureDisabled panel should move onto PageHeader, FormField, ErrorPanel and EmptyState | `App.tsx`, `components/Layout.tsx`, `components/ui.tsx`, `index.css:8`, `api/client.ts`, `auth/session.tsx`, `system/SystemStudio.tsx` |
| `agent/worker-fb94` | New function, Monaco editor, templates, Test tab, version diff | A-05 sub-items, A-20; its inline success text and bare empty lines move onto toasts and EmptyState | `pages/functions/*`, `api/functions.ts`, `package.json` and its lock file, `vite.config.ts`, `tsconfig.json`, `index.css:8` (needs `switch`, which fb92 adds) |

Merge both before package (a) starts, so (a) rewrites `Layout.tsx` and `ui.tsx` once.
