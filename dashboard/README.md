# ForgeBase admin dashboard

React 19, Vite and Tailwind CSS v4, built with [PanelUI web](https://panelui.dev) components.
`npm run build` writes `dist/`, which the server embeds and serves at `/admin/`.

```sh
npm ci
npm run dev      # http://localhost:5173/admin/, proxies /api, /health and /metrics to 127.0.0.1:8090
npm run demo     # http://127.0.0.1:5173/admin/, no server needed: every API call answered in the browser
npm test         # vitest
npm run lint
npm run build
```

## Two ways to run it

- **Against a server** (`npm run dev`): start `forgebase serve` first (it listens on 127.0.0.1:8090), then
  sign in with your administrator. Vite hot-reloads the UI; the API is the real one.
- **Demo mode** (`npm run demo`): the same app with the API answered by the fakes the tests use
  (`src/test/*Mock.ts`, checked against `openapi.json`), so every studio has sample data. Nothing is sent
  anywhere and a reload resets it. You start signed in as `root@example.com` (password `correct-horse`).
  Open `/admin/?first-run` for a fresh server instead: the first-run setup, with the setup token filled
  in, then the Getting started journey. The entry point is `demo/main.tsx`; it is not part of `npm run build`.

## The first journey

A new developer should get from an empty server to a working app without reading docs
([dashboard-ux.md](../docs/requirements/dashboard-ux.md)): create the administrator, a collection with a
record, an API key, upload a file, deploy a function. Overview shows the full checklist; on every other page
the **Getting started** button in the top bar (`components/JourneyGuide.tsx`) shows how far along the
project is and opens the next step in one click. It moves on by itself when a create, save or delete
toast fires (`CHANGED_EVENT` in `src/toaster.ts`), disappears once every step is done, and "Hide this
guide" turns it off for the browser.

## Themes and the accent colour

The theme control is the sun / moon / screen button at the right of the header (and at the top
right of the sign-in page). It holds two choices:

- **Theme**: Light, Dark, or System. System follows the operating system's
  `prefers-color-scheme` and switches live when the system does. System is the default.
- **Accent colour**: Orange (the default; stored as `amber` so older choices keep working), Indigo, Emerald, Rose, Sky or Violet. The accent
  colours primary buttons, links, the active row in the sidebar, focus rings and charts.

Both choices are kept per browser in `localStorage` under `forgebase.admin.theme` and
`forgebase.admin.accent`. Nothing is sent to the server. If storage is blocked (some private
windows), the choice lasts until the page is reloaded.

### How it works

- `src/index.css` defines the theme tokens in shadcn/ui names (`--background`,
  `--foreground`, `--card`, `--muted`, `--border`, `--primary`, `--success`, `--warning`,
  `--info`, `--destructive`, …) for light (`:root`) and dark (`.dark`). Tailwind classes such as
  `bg-card` and `text-muted-foreground` read them, and so do the PanelUI components. Use the
  tokens, not palette classes such as `text-zinc-400`, so a screen works in both themes.
- Each accent preset is a `[data-accent="<id>"]` rule that sets `--primary` and
  `--primary-foreground`, with a light and a dark value. `--ring` and `--chart-1` follow
  `--primary`.
- A small inline script in `index.html` (`#theme-init`) reads the two stored values and sets
  the `dark` class, `data-accent` and `color-scheme` on `<html>` before the first paint, so the
  page never flashes the wrong theme. `src/theme/` keeps `<html>` in step once React runs.
  If a Content-Security-Policy is ever added to `/admin/`, allow that script by its hash.
- Fonts are bundled: Inter Variable for the interface and JetBrains Mono Variable for code, ids and
  snippets (`@fontsource-variable/*`, OFL-1.1, imported in `src/main.tsx`). Vite copies the woff2 files
  into `dist/assets`, so the dashboard still loads nothing from the network; the browser only fetches the
  Latin subsets it needs (about 90 kB). The type scale: 12px (`text-xs`) badges, help and captions, 14px
  (`text-sm`) body, tables, inputs and buttons, 16px (`text-base`) card titles, 24px (`text-2xl`) page titles.
  Use those classes, not `text-[13px]`-style one-offs. The one exception is the 11px uppercase group
  headings in the nav rail (set in `src/index.css`).
- Colours: the warning colour (`--warning`) is a yellow-olive, never the same hue as the orange primary,
  and field outlines (`--input`) meet the 3:1 contrast WCAG asks of a control's boundary.

### Adding an accent

1. Add the id and label to `ACCENTS` in `src/theme/theme.ts`.
2. Add a light and a dark rule for it in `src/index.css`.
3. Add the id to the pattern in the `#theme-init` script in `index.html`.

`src/theme/contrast.test.ts` checks every token pair against WCAG AA (4.5:1) in both themes
and for every accent: body and muted text on each surface, status colours as text and on their
tinted backgrounds, `--primary` as text, and `--primary-foreground` on `--primary`. A new or
changed colour that fails the check fails the test. `src/theme/theme.test.tsx` checks that the
inline script and the app agree on the keys and ids.

### PanelUI

Components are imported from the npm package, for example `import { Table } from
"panelui-web/table"`. Tailwind only scans the PanelUI components the dashboard uses (the
`@source` line in `src/index.css`): add a component's folder there when you import a new one,
or its classes will be missing. Icons come from Hugeicons (`@hugeicons/react` with
`@hugeicons/core-free-icons`).
