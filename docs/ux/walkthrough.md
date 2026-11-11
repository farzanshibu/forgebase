# Dashboard first-journey walkthrough

This is how to check the dashboard against the journey in
[docs/requirements/dashboard-ux.md](../requirements/dashboard-ux.md) ("The test"). The script is
`dashboard/e2e/journey.spec.ts`. It drives a real browser (the installed Google Chrome) against
a real server, takes screenshots in light and dark, and runs an axe accessibility pass on every
studio's main page. The findings it refers to (A-01 and so on) are in [audit.md](audit.md).

## Recipe

1. **Prepare a server.** Use a fresh project and a throwaway administrator. Never use a real
   password or a real email.

   ```sh
   forgebase init /tmp/fb-ux                      # any empty folder
   cd /tmp/fb-ux
   printf '%s\n' "$THROWAWAY_PASSWORD" | forgebase admin create --email admin@example.test --password-stdin
   forgebase serve --port 18110
   ```

   Steps that need unmerged work stay `test.fixme` until it lands:
   - Step 1 (first-run setup) needs a server with no administrator.
   - The other steps need one, which is why the recipe creates it from the command line.

   Once FB-92 is merged, see "Fresh server" below.

2. **Install the runner.** This happens once, in its own folder, and never downloads a browser.

   ```sh
   cd dashboard/e2e
   PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 npm install
   ```

3. **Run.**

   ```sh
   FB_URL=http://127.0.0.1:18110 \
   FB_ADMIN_EMAIL=admin@example.test FB_ADMIN_PASSWORD="$THROWAWAY_PASSWORD" \
   FB_SHOTS=/some/folder/outside/the/repo \
   npx playwright test            # both themes; --project=light or --project=dark for one
   ```

   Screenshots land in `$FB_SHOTS`, named `journey-<step>-<theme>.png`. A failed run leaves a
   trace there too; open it with `npx playwright show-trace <path>`.

4. **Clean up.** Stop the server and delete the project folder. Each run creates resources
   named `ux_notes_<theme>_<run>`, `ux-uploads-<theme>-<run>`, `ux-hello-<theme>-<run>` and
   `ux-token-<theme>-<run>`, so reruns on the same server do not collide.

The dashboard's own `tsc` and `vitest` do not include `e2e/`. It has its own `package.json`, so
the dashboard's dependencies stay unchanged.

## What each step checks

| # | Journey step | Today on main (0.2.0) | Test | Unblocked by |
|---|---|---|---|---|
| 1 | Create the administrator | Command line only; the browser shows a sign-in form (A-01) | `1.` fixme; `1b.` signs in | FB-92 merge, package (e) |
| – | Overview is the starting point | Metrics only (A-02) | fixme | package (b) |
| 2 | Collection with fields, add a record | Works; the record is raw JSON (A-07) | passes | package (c) swaps it to the form |
| 3 | Copy a working snippet | No snippet screen (A-03). An API token reads the collection over HTTP once the scope `collections:read` is known (A-08) | `3.` fixme; `3b.` passes | packages (a), (b), (c), (e) |
| 4 | Upload a file to a bucket | Works; the create form is at the page bottom (A-10, A-11) | passes | package (c) |
| 5 | Write, deploy and test a function that reads the collection | Needs a `.wasm` file (A-05). The test deploys a 128-byte module it builds itself, then calls it | `5.` fixme; `5b.` passes | FB-94 merge, package (d) |
| 6 | See the function's logs; find an error's cause | Logs list works; errors lack the fix and a copyable request id (A-04, A-20) | `6.` passes; `6b.` fixme | packages (a), (d) |
| – | `Ctrl/Cmd+K` palette | Missing (A-06) | fixme | package (a) |
| – | axe on every studio main page | No serious or critical issue, light and dark | 14 × 2 pass | keep green |

**Result on 2026-11-11** against the 0.2.0 release binary: **40 passed, 12 fixme, 0 failed** in
about 1.5 minutes. That is 6 journey tests and 14 accessibility tests per theme; the 12 fixme
are 6 per theme. Every fixme carries the finding id it waits for. When a package lands, remove
its `.fixme`, and the test must pass.

## Clicks from Overview today

The contract says every step is one or two clicks from Overview, with no guessing. Counted by
hand on 0.2.0:

| Step | Path today | Clicks | Guessing needed |
|---|---|---|---|
| 1 | Not in the browser | n/a | You must know `forgebase admin create` |
| 2 | Nav **Database Studio** → **New collection** → form; then **Add record** | 2, then 1 | Field types; the record's JSON shape |
| 3 | Nav **Authentication Studio** → tab **API keys & tokens** | 2 | Scope grammar; that app keys live under Service accounts; no snippet anywhere |
| 4 | Nav **Object Storage Studio** → form at the page bottom → bucket link → **Choose files** | 3 | Where the create form is |
| 5 | Nav **Functions Studio** → **Deploy function** | 2 | How to build a `.wasm` file |
| 6 | Function link → **Logs** | 2 from Functions, 3 from Overview | What "fuel" is; which row is your call |

With packages (a) to (e), Overview's Getting started links each step directly (1 click), and
the palette reaches every create action in 2 keystrokes.

## Fresh server (step 1, after FB-92)

To run step 1, start `forgebase serve` on a project with no administrator. `serve` prints a
line `First run: create the administrator at http://ADDR/admin/setup?token=T`. Then:
- set `FB_SETUP_URL` to that link;
- remove the `.fixme` from step 1 and have it fill the form with `FB_ADMIN_EMAIL` and
  `FB_ADMIN_PASSWORD`.

The later steps then sign in with the same values. Package (e) owns that change.

## Screenshot set from the audit

These were taken with a separate crawler (not committed), at 1280×860 unless the name ends in
`-w768` or `-w1024`. They are kept outside the repo in
`/Volumes/Vinu1TBSSD/munder-difflin/.cache/ux-audit/`:

- `anon-overview-{light,dark}.png`: sign-in on a server with no administrator.
- `empty-<route>-{light,dark}.png`: 25 routes on a fresh server (every studio, the main tabs,
  and the 404 page).
- `data-<route>-{light,dark}.png`: 25 routes after the journey plus seeded key-value keys, a
  vector collection, graph nodes, a queue topic with a message, and a user. Includes detail
  pages for a collection, bucket, function, function logs, vector search and topic.
- `data-*-w768.png` and `data-*-w1024.png`: narrow layouts.
- `journey-*-{light,dark}.png`: from the e2e script.
- `report-{anon,empty,data}.json`: per-route axe results (serious and critical only), page
  text, and browser console errors.

## Screens added by FB-100 to FB-102 (what to look at)

The journey script covers the first run. These screens came after it. Check each by hand
on a server with some data, in light and dark, and at 768 px wide. The unit tests in
`dashboard/src` cover their behavior; this list covers what a person sees.

**Sign-in (FB-137).** Two choices under "Sign in as": *Instance administrator* (the account made at
setup, signs in at `/api/auth/admins/login`) and *Realm administrator* (a normal user of one realm who
holds `identity:admin` there; adds a Realm field and signs in at `/api/auth/login`). A realm
administrator's nav has no System, Billing or Features and settings; opening one by address
shows "is for platform administrators" with the realm name.

**Data studios (FB-100).**
- Database: header with **New collection**; a collection opens with **Add record** and **Use from code**.
  Grid filter, sort, page and the open record are in the address (`?filter=&sort=&page=&record=`).
  The record form has one control per field type, required marks, errors under the field and an
  **Edit as JSON** switch. Delete asks for the collection's name. SQL: **Show secrets**.
- Storage: **New bucket** in the header (also `?new=1`), optional Tenant, a Scope column, **Rules**
  tab per bucket, quota says "No quota applies to administrators" for an administrator.
- Key-value: **New key** with TTL, **Rules** tab for the namespace, Flush asks for the namespace name.
- Vector: **New collection** shows the request it sends, **Load sample**; Graph: **New node** and edge
  sheets with a live request body, **Load sample**.
- Rules (all five studios and Queues): one editor, Locked / Everyone / Expression per operation, the
  `@request` help, **Save rules**.

**Functions, jobs, queues (FB-101).**
- Functions: **New function** is one page (template cards, settings, source). **Test** shows the
  request id with **Copy request id** and **Find in logs**. Any error panel with a request id links
  **Find in logs** to *Functions › Find a request in the logs* (`/functions/logs?request=<id>`), which
  lists the function runs that carried it.
- Jobs: tenant and creator columns, a Workers panel, the reserved-kind explanation, confirm
  dialogs. Queues: **New topic** with tenant, **New subscription**, rules for topics and subscriptions.

**Users, access, billing, system (FB-102).**
- Authentication: tab **API keys** (keys for apps, then personal tokens); **Create API key** is a side sheet (`?new=1`) that ends
  with the one-time secret and a curl example; Service accounts have New, Delete-by-name and a Keys panel.
- Access and Identity: Realm administrators panel, organization delete by slug, Client apps for platform
  administrators only.
- Billing: four groups (Catalog, Customers, Money, Settings), each with its own page tabs.
- System: health plus the database card (backend, mode, version, extension features active, portable
  or off); **Features and settings** is one form with one Save.

Known gap: the server has no delete-realm route, so the dashboard cannot delete a realm.
