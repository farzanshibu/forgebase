# Dashboard UX work packages (from the FB-97 audit)

These five packages close the findings in [audit.md](audit.md). Each package owns its files
outright; no two packages edit the same file. Paths are under `dashboard/`.

## Order

1. **Merge `agent/worker-fb92` (first-run setup) and `agent/worker-fb94` (function editor)
   first.** Both touch files the packages below rewrite (`App.tsx`, `components/Layout.tsx`,
   `components/ui.tsx`, `index.css`, `pages/functions/*`).
2. **Package (a) goes next, alone.** Packages (b) to (e) use its shared components: PageHeader,
   EmptyState, ErrorPanel, FormField, UseFromCode, ConfirmDialog, CreateSheet, Skeleton, Pager,
   HelpTip and the toast helpers. They cannot finish their findings without them.
   - To shorten the wait, (a) can land its component APIs (props and types, with simple bodies)
     in its first commit. (b) to (e) can then branch from that commit while (a) finishes the
     shell (palette, nav, breadcrumbs).
3. **Packages (b), (c), (d) and (e) then run in parallel.** Their file sets are disjoint.

## Shared rules for every package

- **Shared files.** Use the shared components from (a); do not fork them. If one is missing a
  prop, ask the (a) owner, or god after (a) has merged. Do not add a local copy.
- **Test helpers.**
  - (a) owns `src/test/helpers.tsx`, `src/test/mockApi.ts` and `src/test/setup.ts`.
  - The other packages add new mock files only, named `src/test/<package>Mock.ts`. Their
    existing studio mocks (for example `src/test/storageMock.ts`) belong to the package that owns
    that studio.
- **E2E.** `dashboard/e2e/journey.spec.ts` marks each step waiting on a package with
  `test.fixme` and a finding id. A package removes the `.fixme` from its own steps only. Each
  is a one-word edit, so merges do not conflict. Every package must leave `npx playwright test`
  green (see [walkthrough.md](walkthrough.md)).
- **Gates for every package:** `npm run build`, `npm run lint`, `npm test`. Run the e2e journey
  and its accessibility pass against a live 0.2.x server in light and dark, and keep zero
  serious or critical axe issues.
- **Copy.** Use sentence case and plain words, and spell out each term the first time it
  appears or give it a HelpTip. Keys always appear as `<YOUR_API_KEY>`.

---

## (a) App shell and shared patterns (goes first)

**Owns:**
- **Changed files:** `src/App.tsx`, `src/components/Layout.tsx`, `src/components/ui.tsx`,
  `src/components/controls.tsx`, `src/studios.ts`, `src/toaster.ts`, `src/hooks.ts`,
  `src/index.css`.
- **Deleted:** `src/components/StudioPlaceholder.tsx`.
- **New components in `src/components/`:** `PageHeader.tsx`, `EmptyState.tsx`,
  `ErrorPanel.tsx`, `FormField.tsx`, `HelpTip.tsx`, `UseFromCode.tsx`, `ConfirmDialog.tsx`,
  `CreateSheet.tsx`, `Skeleton.tsx`, `Pager.tsx`, `CommandPalette.tsx`, `Breadcrumbs.tsx`.
- **New modules in `src/`:** `snippets.ts`, `errors.ts`, `glossary.ts`, `validate.ts`.
- **Tests and helpers:** their unit tests in `src/components/*.test.tsx` and
  `src/snippets.test.ts`, plus `src/test/helpers.tsx`, `src/test/mockApi.ts`,
  `src/test/setup.ts`, `src/App.test.tsx` and `src/components/ui.test.tsx`.

**Closes:**
- A-06 (palette, breadcrumbs, three groups).
- A-14 (names).
- A-04 (ErrorPanel, FormField, `errors.ts`).
- A-17 (`studios.ts` clean-up).
- A-25 (keep the accessibility pass).
- The component halves of A-03, A-09, A-10, A-11, A-12, A-13, A-15, A-19 and A-24.

**Scope:**
- **`studios.ts`.** Each studio gets `label`, `title`, `summary`, `group`
  (`build | users | operate`), `docs` and an optional `feature` (the fb92 feature id, so the nav
  and the palette hide disabled studios through one filter). Remove `phase` and `features`.
- **`Layout.tsx`:**
  - Overview first, then three groups: Build, Users and access, Operate.
  - The top bar holds Breadcrumbs, a "Search ⌘K" button, the account and the theme menu.
  - Drop the duplicated label and summary.
- **`CommandPalette.tsx`:**
  - Opened by `Ctrl/Cmd+K` and the button.
  - Sections: Go to (studios and tabs), Create (New collection, New bucket, New function,
    Create API key, New topic), and Resources (collections, buckets, functions, topics and
    vector collections, fetched when the palette opens and cached for 30 s).
  - Arrow keys move, Enter goes, Escape closes. The palette is announced as a dialog with a
    combobox.
- **`ErrorPanel.tsx`:**
  - A plain title from `errors.ts` (code → `{ title, next }`), falling back to the server
    message.
  - A next step, "Copy request id", and "Details" with the raw JSON.
  - 401 and 403 get separate wording.
  - It exports `fieldErrors(error)` for FormField.
- **`UseFromCode.tsx` and `snippets.ts`:**
  - Tabs JavaScript, Rust and curl, each with a copy button.
  - Generators for collection, record, bucket, object, function, topic, subscription, job kind,
    key-value namespace, vector collection and graph.
  - JS uses `@forgebase/sdk` (`new ForgeBase(origin)`), falling back to `fb.request` where there
    is no wrapper. Rust uses `forgebase-sdk`.
- **`ConfirmDialog.tsx`.** Props `title`, `consequence`, `confirmLabel` and an optional
  `confirmText` that must be typed exactly. Keep `ConfirmButton` as a wrapper so studios move
  over gradually.
- **`toaster.ts`.** Add `notify.saved(what)`, `notify.created(what, href?)` and
  `notify.failed(error)`.
- **`controls.tsx`.** Add PanelUI `Check` and `Toggle` wrappers with the same value-in,
  value-out API as `Sel`.

**Definition of done:**
- Every studio's main page uses `PageHeader`, and the nav shows three groups.
- `Ctrl/Cmd+K` reaches every studio and the four create actions.
- `ErrorView` re-exports `ErrorPanel`, so every existing screen gets the new error display with
  no studio edit.
- The storybook-free demo route `/_patterns`, which shows each component, exists in dev builds
  only.

**Tests:**
- Unit tests for:
  - EmptyState: renders its action and docs link.
  - ErrorPanel: maps codes, copies the request id, hides Details by default, words 401 and 403
    differently.
  - UseFromCode: never contains a real key, and contains the origin and the name in all three
    tabs.
  - CommandPalette: opens on Cmd+K and Ctrl+K, filters, Enter navigates, Escape closes.
  - ConfirmDialog: the confirm button stays disabled until the text matches.
  - FormField: `aria-describedby` points at the error.
  - Pager.
- E2E: remove the `.fixme` from "command palette jumps to the new collection".

---

## (b) Overview and onboarding

**Owns:** `src/pages/OverviewPage.tsx`, `src/pages/OverviewPage.test.tsx`,
`src/pages/LoginPage.tsx`, new `src/pages/overview/GettingStarted.tsx`,
`src/pages/overview/ConnectApp.tsx`, `src/pages/overview/overview.test.tsx` and
`src/test/overviewMock.ts`.

**Closes:**
- A-02 (Getting started, Connect your app).
- A-16 ("not reported" and "collecting…").
- The login hint in A-01.

**Scope:**
- **GettingStarted:**
  - Five steps, each read from the server: administrator, collection (not a system one), API
    key, uploaded file, deployed function.
  - Each step links in one click to the place to do it: `/database/new`, the Create API key
    dialog, `/storage`, `/functions/new`.
  - It can be collapsed (per browser) and hides itself once all steps are done, with a
    "Show again" link.
- **ConnectApp:**
  - The server URL with a copy button.
  - A "Create an API key" button that opens the dialog from (e) through a route, `/auth/keys?new=1`.
  - A `UseFromCode` block with the install command and a first `list()` against the newest
    collection, or a `posts` placeholder when there is none.
- **Metrics:**
  - The cards move under a "Server health" heading below.
  - A card the server does not report collapses into a single muted line.
  - The header copy loses "scrapes".
- **LoginPage:** add one line for servers set up from the command line (A-01). The setup
  redirect itself comes from fb92.

**Definition of done:**
- A fresh server shows Getting started with step 1 done and four links.
- After the journey, all five are done.
- Every journey step is one or two clicks from Overview.
- No card says "not reported" next to "collecting…".

**Tests:**
- Unit tests: each step's state from mocked lists; collapse persists; snippets contain the
  origin.
- E2E: remove the `.fixme` from "Overview shows Getting started and Connect your app" and from
  "3. copy a working snippet…". The second also needs (c)'s collection panel, so flip it when
  both have merged.

---

## (c) Data studios

**Owns:**
- `src/pages/database/*`, `src/pages/storage/*`, `src/pages/redis/*`, `src/pages/vector/*`,
  `src/pages/graph/*`, `src/api/storage.ts`.
- `src/storage-redis.smoke.test.tsx`, `src/test/storageMock.ts`, `src/test/kvMock.ts`,
  `src/test/vgMock.ts`.

**Closes:**
- A-07 (record form).
- A-18 (grid types, filter help, URL state).
- A-21 (entitlements 400).
- A-22 (the Vector tab row).
- In these studios, the adoption halves of A-03 (Use from code on collection, bucket, key-value
  namespace, vector collection and graph), A-09, A-10, A-11, A-12, A-13, A-15, A-19 and A-24.

**Scope:**
- **`RecordForm.tsx`.** Typed controls per field type, an "Edit as JSON" toggle, and field
  errors under their fields. It opens in a CreateSheet from "Add record" and from a row click.
- **SchemaDesigner:**
  - HelpTips on Type (`base`: plain data; `auth`: users who can sign in; `view`: read-only
    query) and on Schema mode (`strict`: unknown fields rejected; `flexible`: extra fields kept).
  - Typed option inputs instead of "Options (JSON)".
  - The name is validated as you type.
  - Delete asks for the collection name.
- **Storage:**
  - "New bucket" moves to the PageHeader primary action, opening a CreateSheet.
  - "Also delete its objects" moves into each bucket's ConfirmDialog.
  - The upload area gets an EmptyState ("Drop files here or choose files").
  - Quota uses a subject, or no call for administrators.
- **Redis:** rename per Q1, add the namespace to the URL, and make "Flush namespace" ask for the
  namespace name.
- **Vector and graph:**
  - EmptyState with "Load sample" (a three-dimension sample, as in the audit's seed data).
  - Body examples in the create forms, so a parser error such as "unknown field `dimension`"
    cannot happen from the UI.
  - Drop the single-tab row.

**Definition of done:** journey steps 2 and 4 take no guessing. A record is added through typed
fields; a bucket is created from the header and a file dropped into it. Each data resource shows
Use from code. No bare "No X yet." line is left in these folders.

**Tests:**
- Unit tests:
  - RecordForm renders one control per field type and maps server field errors.
  - Grid filter and page round-trip through the URL.
  - Bucket delete needs the typed name.
  - Storage makes no entitlements call without a subject.
- E2E: step 2 switches from the JSON textarea to the form (update the two lines that fill
  `Record JSON`).

---

## (d) Runtime studios

**Owns:** `src/pages/functions/*`, `src/pages/jobs/*`, `src/pages/queues/*`,
`src/api/functions.ts`, `src/api/jobs.ts`, `src/api/queues.ts`, `src/test/fnMock.ts`,
`src/test/tasksMock.ts`, `src/test/queueMock.ts`.

**Closes:**
- A-05 (the remaining editor gaps).
- A-20 (logs).
- A-22 (the Functions tab row).
- In these studios, the adoption halves of A-03 (function, topic, subscription and job kind),
  A-09, A-10, A-11, A-12, A-13, A-15 and A-19.

**Scope (after fb94 merges):**
- **Templates.** Add a "Read a collection" HTTP template with a collection picker and `db.read`
  on. The webhook template asks for its collection instead of assuming `orders`.
- **Deploy.** Deploy shows a toast and opens the Test tab. "Deploy function" becomes a secondary
  "Upload .wasm" (or per Q3).
- **Test panel.** It sends an `x-request-id` and shows that run's logs only. The request id has a
  copy button, and errors use ErrorPanel.
- **Logs:**
  - A `?request=` filter, copy buttons and a HelpTip on fuel.
  - An error row shows ErrorPanel.
  - ErrorPanel's "Find in logs" deep-links here.
- **Queues.** "New topic" moves to the PageHeader. Ack deadline, dead-letter topic, ordering key
  and dedup id get HelpTips. Delete topic asks for the name.
- **Jobs.** Its summary drops "Queues". Schedules gain an EmptyState that explains cron
  expressions.

**Definition of done:** journey steps 5 and 6 work with no file upload. The developer starts from
the "Read a collection" template, deploys, runs it in Test, sees its logs, and finds the cause
of a forced error from the message and the request id alone.

**Tests:**
- Unit tests: the template list contains a db.read template; deploy navigates to Test; logs
  filter by request id.
- E2E: remove the `.fixme` from "5. write, deploy and test a function…" and "6b. an error
  explains its fix…". Keep 5b (WASM upload) as a regression test.

---

## (e) Users, access, billing and system

**Owns:**
- `src/pages/auth/*`, `src/pages/access/*`, `src/pages/identity/*`, `src/pages/billing/*`,
  `src/pages/system/*`, `src/pages/SystemPage.tsx`, `src/pages/SystemPage.test.tsx`.
- `src/pages/setup/*` (after fb92 merges).
- `src/e2e.billing.smoke.test.tsx`.
- `src/test/idMock.ts`, `src/test/billingMock.ts`, `src/test/backupMock.ts`,
  `src/test/setupMock.ts`, `src/pages/access/accessMock.ts`, `src/pages/auth/authMock.tsx`.

**Closes:**
- A-08 (Create API key dialog).
- A-17 (stale System copy).
- A-23 (Billing tab groups).
- The fb92 follow-ups in A-01.
- In these studios, the adoption halves of A-09, A-10, A-11, A-12, A-13, A-15 and A-24.

**Scope:**
- **`pages/auth/CreateKeyDialog.tsx` and `scopes.ts`:**
  - Pick or create a service account, then pick scopes grouped by resource (the scope string
    shows under each), then an expiry.
  - The key is shown once beside a curl snippet.
  - It opens from `/auth/keys?new=1`, so Overview and the palette can link to it.
  - The tab is renamed "API keys", and personal tokens become a secondary section.
- **Setup and features (fb92):**
  - Password rule help, and the confirm field checked when it loses focus.
  - `aria-describedby` on field errors.
  - Docs links and badge tooltips on the feature cards.
  - Settings become one form with a toast in System; the wizard is kept for first run only.
  - Fix the `f.enabled` bug.
- **System.** Replace the "arrive in phase 9" paragraph with links to Backups, the Audit log and
  Features and settings.
- **Billing.** Group the 13 tabs into four; HelpTip on "Dunning".
- **Access and identity:**
  - EmptyState for organizations, client apps, realms and domains.
  - HelpTips for ACL, OIDC, realm, consent and role set.
  - Deleting a realm or an organization asks for its name.
  - Drop the three local `Empty` components.

**Definition of done:**
- A developer creates an app key with read access to one collection in one dialog, without
  knowing the scope grammar.
- First-run setup passes contract section 4 and rules 4, 6 and 9 for its forms.
- No "phase" copy remains.

**Tests:**
- Unit tests: CreateKeyDialog builds `collections:read:<name>` from the checkboxes and shows the
  secret once; the setup confirm field validates on blur; the Billing groups route correctly.
- E2E: remove the `.fixme` from "1. first-run setup creates the administrator in the browser".
  It needs a fresh server with no administrator (see walkthrough.md, "Fresh server").
- E2E step 3b switches to the dialog.

---

## File ownership at a glance

| Package | Owns (under `dashboard/src/`) |
|---|---|
| a | `App.tsx`, `App.test.tsx`, `components/**`, `studios.ts`, `toaster.ts`, `hooks.ts`, `index.css`, `snippets.ts`, `errors.ts`, `glossary.ts`, `validate.ts`, `e2e.smoke.test.tsx`, `live.smoke.test.ts`, `test/helpers.tsx`, `test/mockApi.ts`, `test/setup.ts`, `test/contract.ts` |
| b | `pages/OverviewPage*.tsx`, `pages/overview/**`, `pages/LoginPage.tsx`, `test/overviewMock.ts` |
| c | `pages/{database,storage,redis,vector,graph}/**`, `api/storage.ts`, `storage-redis.smoke.test.tsx`, `test/{storage,kv,vg}Mock.ts` |
| d | `pages/{functions,jobs,queues}/**`, `api/{functions,jobs,queues}.ts`, `test/{fn,tasks,queue}Mock.ts` |
| e | `pages/{auth,access,identity,billing,system,setup}/**`, `pages/SystemPage*.tsx`, `e2e.billing.smoke.test.tsx`, `test/{id,billing,backup,setup}Mock.ts` |

Files nobody owns stay untouched by these packages: `api/client.ts`, `api/index.ts`, the other
`api/*.ts`, `auth/**`, `theme/**` and `main.tsx`. If one must change, the (a) owner makes the
change.

## Questions for the human

Each question lists options; the first is the recommendation.

- **Q1. What should the key-value studio be called?** It speaks the Redis protocol, but it is
  ForgeBase's own engine. Overview already calls it "Key-value store".
  - (a) **"Key-value"**, with the summary "Redis compatible".
  - (b) Keep "Redis".
  - (c) "Cache and key-value".
- **Q2. Where should the "Docs" links on each page point?**
  - (a) **The repo's docs on GitHub at the running version's tag** (for example
    `…/blob/v0.2.0/docs/database.md`). This is stable and needs no new hosting.
  - (b) The same docs on `main`, which are always newest but may describe unreleased features.
  - (c) Serve the docs from the binary at `/docs`. This works offline but makes the binary
    bigger and adds a build step.
- **Q3. After the function editor lands, what happens to raw `.wasm` upload?**
  - (a) **Keep it as a secondary "Upload .wasm" action** next to "New function".
  - (b) Hide it under an "Advanced" menu.
  - (c) Remove it from the dashboard; the command line can still deploy.
