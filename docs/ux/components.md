# Dashboard shared components (FB-98)

Which component to use for which job, for packages FB-99 to FB-102. The bar is
[dashboard-ux.md](../requirements/dashboard-ux.md). Every file in
`dashboard/src/components/` starts with a usage example; `/admin/_patterns`
(dev builds only, `npm run dev`) shows each component in both themes.

**Rule:** use these; do not fork them or add a local copy. If one lacks a prop,
ask god (FB-98 has merged) and it is added here.

## Pick by job

| Job | Use | Import from |
|---|---|---|
| Top of a studio page: name, one sentence, docs link, primary action | `PageHeader` (its studio's glossary term shows as a "?" after the title; `term` overrides, `term={null}` hides) | `components/PageHeader` |
| An empty list | `EmptyState` | `components/EmptyState` |
| Any error | `ErrorPanel` (also `ErrorView`); with a request id it links "Find in logs" (`/functions/logs?request=<id>`) | `components/ErrorPanel` or `components/ui` |
| Who may do what with a resource (namespace, vector collection, graph label or edge type, bucket, queue topic or subscription) | `ResourceRules kind name noun`, one panel for the database, storage, vector, graph, queue and key-value studios | `components/ResourceRules` (types and routes in `components/resourceRulesApi`) |
| An error for one field | `FormField` `error`, fed by `fieldErrors(error)` | `components/FormField`, `errors` |
| A term that needs explaining (TTL, RLS, ACL…) | `HelpTip` with a `glossary.ts` term | `components/HelpTip` |
| "How do I call this from my app?" | `UseFromCodeButton` in the header, or `UseFromCode` inline | `components/UseFromCode` |
| Delete, flush, revoke | `ConfirmDialog` (`confirmText` = the name when it cannot be undone) | `components/ConfirmDialog` |
| Create a thing | `CreateSheet`, opened by the header's primary action and by `?new=1` | `components/CreateSheet`, `hooks` |
| Row details or an edit form beside the list | `DetailSheet` | `components/CreateSheet` |
| Loading a list, table or cards | `SkeletonTable`, `SkeletonCards`, `SkeletonLines` | `components/Skeleton` |
| Small inline wait (a button, one value) | `Loading` | `components/ui` |
| Paging a table | `Pager` (Previous and Next with "1–25 of 130") | `components/Pager` |
| Paging a stream (logs, objects, messages, keys) | `Pager mode="more"` with `useCursorList` | `components/Pager`, `hooks` |
| A save, create, delete or copy happened | `notify.saved / created / deleted / copied` | `toaster` |
| A save failed and nothing on screen can show it | `notify.failed(error)` | `toaster` |
| Checkbox, switch, select | `Check`, `Toggle`, `Sel` (`aria-label` is the accessible name, also next to a visible `label`) | `components/controls` |
| Section tabs | `RouteTabs` (router driven) | `components/controls` |
| Validate a name as the user types | `validate.collectionName`, `bucketName`, `functionName`, `topicName`, `namespaceName`, `json`, `integer`, `required`, `all` | `validate` |
| Better breadcrumb names on a detail page | `useCrumbs([...])` | `components/crumbs` |

## The page skeleton

Every studio main page looks like this. The top bar already shows the
breadcrumbs, so the page does not draw "← All topics" links or repeat the name.

```tsx
import { PageHeader } from "../../components/PageHeader";
import { Button } from "../../components/ui";
import { RouteTabs } from "../../components/controls";
import { useCreateParam } from "../../hooks";

export default function QueuesPage() {
  const [creating, setCreating] = useCreateParam(); // the palette's "New topic" opens it too
  return (
    <div>
      <PageHeader studio="queues" primary={<Button tone="primary" onClick={() => setCreating(true)}>New topic</Button>} />
      <RouteTabs label="Queues sections" tabs={TABS} className="mt-4" />
      <div className="mt-6">{/* routes */}</div>
      <NewTopicSheet open={creating} onOpenChange={setCreating} />
    </div>
  );
}
```

`studio="queues"` fills the title, the sentence and the docs link from
`studios.ts`. Pass `title`, `description` or `docs` to override; `docs={null}`
hides the link. A detail page uses the resource as the title:

```tsx
<PageHeader title={topic} description="Topic" docs="docs/adr/0022-queues-pubsub.md"
  secondary={<UseFromCodeButton kind="topic" name={topic} />}
  primary={<Button tone="primary" onClick={…}>Publish</Button>} />
```

## Lists

```tsx
const list = useLoad(listBuckets, "buckets");
<ErrorPanel error={list.error} onRetry={list.reload} />
{list.loading && !list.error && <SkeletonTable rows={4} columns={5} label="Loading buckets" />}
{list.data?.length === 0 && (
  <EmptyState
    title="No buckets yet"
    why="Buckets hold files your app uploads, like avatars or exports."
    action={<Button tone="primary" onClick={() => setCreating(true)}>New bucket</Button>}
    docs="docs/adr/0018-object-storage.md"
  />
)}
{list.data && list.data.length > 0 && <Table aria-label="Buckets">…</Table>}
```

- A filtered list with no match: `<EmptyState compact title="No users match" why="Clear the search to see every user." />`.
- Starter copy for the big empty states is in [audit.md A-09](audit.md).

## Create

```tsx
const [name, setName] = useState("");
const [touched, setTouched] = useState(false);
const [saveError, setSaveError] = useState<unknown>();
const nameError = (touched ? validate.bucketName(name) : undefined) ?? fieldErrors(saveError).name;

<CreateSheet
  open={creating} onOpenChange={setCreating}
  title="New bucket" description="Buckets hold files your app uploads."
  submitLabel="Create bucket"
  canSubmit={!validate.bucketName(name)}
  onError={setSaveError}
  onSubmit={async () => {
    await createBucket({ name });
    list.reload();
    return { what: `Bucket ${name}`, href: `/storage/b/${encodeURIComponent(name)}` };
  }}
>
  <FormField label="Name" required help="Lower-case letters, digits, dots and dashes." error={nameError}>
    {(p) => <Input {...p} value={name} onChange={(e) => { setName(e.target.value); setTouched(true); }} />}
  </FormField>
  <Toggle checked={versioning} onChange={setVersioning} label="Versioning" description="Keep earlier versions of each file." />
</CreateSheet>
```

- Success closes the sheet, toasts "Bucket avatars created" and opens `href`.
- With `onError`, the sheet's ErrorPanel hides the field list, because the
  fields show their own errors. Without it, the panel lists them.
- The palette's Create actions go to `/storage?new=1`, `/queues?new=1`,
  `/auth/keys?new=1`, `/database/new` and `/functions/new` (FB-94). A page
  that uses `useCreateParam()` opens its sheet from `?new=1`.

## Delete

```tsx
<ConfirmDialog
  trigger={<Button tone="danger">Delete</Button>}
  title={`Delete bucket ${b.name}?`}
  consequence="Every file in it and every earlier version is deleted. Apps that read it get 404."
  confirmLabel="Delete bucket"
  confirmText={b.name}
  onConfirm={async () => { await deleteBucket(b.name, alsoFiles); notify.deleted(`Bucket ${b.name}`); list.reload(); }}
>
  <Check checked={alsoFiles} onChange={setAlsoFiles} label="Also delete its files" />
</ConfirmDialog>
```

- Use `confirmText={name}` for collection, bucket, function, topic, namespace
  flush, realm and organization (audit A-13).
- `onConfirm` may return a promise: the dialog waits, shows a thrown error in
  place and closes on success.
- `ConfirmButton label="Delete x" onConfirm={…}` still works; its action reads
  "Confirm delete x". Move to ConfirmDialog when you touch the screen, adding
  `consequence` and `confirmText`.

## Errors

- `ErrorPanel error={e}` turns an `ApiError` into a plain title and a "What to
  do" line from `errors.ts`, shows the request id with a copy button, and hides
  the raw JSON under Details.
- 401 reads "Your session ended". 403 reads "You do not have permission to do
  this" and names the missing permission; it never says "sign in".
- `feature_disabled` shows where to switch the feature back on.
- A new error code gets its words in `ERROR_WORDS` in `errors.ts`, never in a page.
- In a form, map `fieldErrors(error)` into each `FormField` and pass
  `fields="hide"` to the panel.

## Use from code

`UseFromCodeButton kind name` in the detail page header opens a side sheet with
JavaScript, Rust and curl tabs, each with a copy button. The snippets use
`location.origin` and `<YOUR_API_KEY>`; a real key never appears.

| Screen | `kind` | `name` |
|---|---|---|
| Collection | `collection` | collection name |
| Record | `record` | collection name (`snippets.record(url, c, id)` for one record) |
| Bucket | `bucket` | bucket name |
| Object | `object` | `bucket/key` |
| Function | `function` | function name |
| Topic / subscription | `topic` / `subscription` | its name |
| Job kind | `jobKind` | kind |
| Key-value namespace | `kvNamespace` | namespace |
| Vector collection | `vectorCollection` | name (`snippets.vectorCollection(url, name, field, dim)` to match the field) |
| Graph | `graph` | node label, or "" |

For custom arguments pass the snippet itself:
`<UseFromCode snippet={snippets.vectorCollection(serverUrl(), name, field, dim)} />`.

## Plain words

- Wrap a term in `HelpTip` the first time a screen shows it:
  `<label>TTL <HelpTip term="ttl" /></label>`, or `FormField term="ttl"`.
- Terms live in `glossary.ts`. Add one there; do not pass free text.
- Studio names come from `studios.ts`: Overview, Database, Storage, Functions,
  Key-value, Vector, Graph, Queues, Jobs, Authentication, Access, Identity,
  Billing, System, Features and settings. No "Studio" suffix.

## Feedback

- Call `notify.saved("Schema")` after every save instead of an inline
  "Saved." line. Use `notify.created`, `notify.deleted` and `notify.copied` the
  same way.
- Use Skeletons for anything that is a list, table or card grid. Keep `Loading`
  for a small inline wait.
- Validate names as the user types with `validate.ts`. The server stays the
  judge.

## The shell (FB-98 owns it)

- `studios.ts` holds one record per studio: `label`, `title`, `summary`,
  `group`, `docs`, an optional fb92 `feature` id, an optional glossary `term`
  (PageHeader shows it as a "?" after the title), and `tabs` for the palette.
  Tab labels are the words on the page's own tabs (Authentication: "API keys"; Billing: its four groups Catalog, Customers, Money, Settings).
  If you add a tab to a studio's RouteTabs, add it to `tabs` too, so the palette
  finds it.
- `visibleStudios(features, { platform })` hides a switched-off studio from the
  nav and the palette, along with its Create action and its resources. With
  `platform: false` (a realm administrator, `useAdminScope()`) it also hides the
  platform-only studios in `PLATFORM_ONLY`: Billing, System and Features and
  settings. The shell passes the scope; do not filter again in a page.
- Breadcrumbs come from the route. Detail routes follow `/<studio>/<kind>/<name>`
  (`c`, `b`, `f`, `t`, `s`, `j`, `w`); keep to it, or call `useCrumbs`.
- `docsHref(doc, version)` links to `github.com/farzanshibu/forgebase/blob/v<version>/<doc>`,
  and to `main` for a dev build (decision Q2).

## Toasts

`toaster.add` shows one card per action: an identical toast (same title,
description and type) added again within 600 ms returns the first one. PanelUI
writes each toast's words twice in the DOM (a hidden spacer and the visible
text), so a test that counts `getByText` hits sees two for one toast. Count
`[data-slot="toast"]` instead, or wait with `toast()` from
`test/dataStudioMock.ts`.

## Tests

- `src/test/setup.ts` waits up to 8 s in `findBy*` and 30 s per test, because
  lazy studio chunks load slowly in a full parallel run.
- Shared-component tests are in `src/components/*.test.tsx` and
  `src/snippets.test.ts`. Studio tests find the shared UI by role and name:
  - `alert` for ErrorPanel, with "What to do" and a "Copy request id" button;
  - `alertdialog` for ConfirmDialog;
  - `form` named by the sheet's title for CreateSheet;
  - `region` named by its title for EmptyState;
  - `status` named by its label for a Skeleton;
  - `navigation` named "Pages" or "More results" for Pager.
