# Dashboard UX and developer experience (FB-97 onward)

Human request (2026-11-11): "the UI / UX should be properly top notch, easy to
develop with, easy to understand, and a straightforward approach."

This is the bar every dashboard screen is held to, existing and new. Change it
only through god.

## The test

A developer who has never seen ForgeBase opens the dashboard and, without
reading the docs, can in about ten minutes:

1. create the administrator (first-run setup),
2. create a collection with a few fields and add a record,
3. copy a working snippet that reads that collection from their app
   (JavaScript SDK, Rust SDK or curl),
4. upload a file to a bucket,
5. write, deploy and test an edge function that reads the collection,
6. see the function's logs, and find any error's cause from the message alone.

Every step is reachable in one or two clicks from Overview, and nothing on the
way needs guessing.

## Rules for every screen

1. **Say what it is.** A page header with the studio name, one plain sentence
   on what it does, a docs link, and the primary action at the top right.
2. **Empty states teach.** An empty list says what the thing is, why you would
   create one, and offers the primary action (and a sample where it helps), never
   just "No items".
3. **Use from code.** Every resource you create (collection, record, bucket,
   function, queue topic, job kind, key-value namespace, vector collection,
   realtime channel) shows a "Use from code" panel with copyable JavaScript SDK,
   Rust SDK and curl snippets, filled in with the server URL and the resource
   name. Keys appear as placeholders such as `<YOUR_API_KEY>`, never real values.
4. **Errors explain the fix.** An error shows the message in plain words, what
   to do next, and the request id with a copy button. No raw JSON dumps unless
   the user opens "Details". Form errors appear next to their field.
5. **One way to do each thing.** Lists, filters, pagination, detail drawers,
   forms, confirms and toasts look and behave the same in every studio (one
   PanelUI component per pattern). Destructive actions confirm; irreversible ones
   ask the user to type the name.
6. **Fast feedback.** Skeletons while loading, a toast on every save, inline
   validation before submit, and optimistic updates where safe.
7. **Find anything.** The nav rail is grouped (Build, Users and access,
   Operate). `Ctrl/Cmd+K` opens a command palette to jump to any studio,
   resource or action. Breadcrumbs on detail pages. Deep links work.
8. **Plain words.** Sentence case, no unexplained jargon (a tooltip or help text
   spells out terms such as RLS, ACL, TTL, OIDC), consistent names for the same
   thing everywhere.
9. **Accessible and themed.** Keyboard reachable, visible focus, labels on
   every control, WCAG AA contrast, light and dark themes, usable from 1024 px
   wide and readable at 768 px.

## Overview is the starting point

- A **Getting started** checklist that tracks the journey above (it reads real
  state: admin exists, a collection exists, an API key exists, a function is
  deployed) and links each step.
- A **Connect your app** panel: server URL, how to create an API key, and SDK
  install plus first-query snippets.
- Health, version and recent activity stay below.

## How it is checked

- A browser walkthrough of the journey (Playwright against a real server, using
  the installed Chrome) runs in CI or as a documented script, with screenshots in
  light and dark.
- An automated accessibility check (axe) reports no serious or critical issue
  on each studio's main page.
- Unit tests cover the shared patterns (empty state, error panel, use-from-code
  panel, command palette).
