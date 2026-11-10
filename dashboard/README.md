# ForgeBase admin dashboard

React 19, Vite and Tailwind CSS v4, built with [PanelUI web](https://panelui.dev) components.
`npm run build` writes `dist/`, which the server embeds and serves at `/admin/`.

```sh
npm ci
npm run dev      # http://localhost:5173/admin/, proxies /api, /health and /metrics to 127.0.0.1:8090
npm test         # vitest
npm run lint
npm run build
```

## Themes and the accent colour

The theme control is the sun / moon / screen button at the right of the header (and at the top
right of the sign-in page). It holds two choices:

- **Theme**: Light, Dark, or System. System follows the operating system's
  `prefers-color-scheme` and switches live when the system does. System is the default.
- **Accent colour**: Amber (the default), Indigo, Emerald, Rose, Sky or Violet. The accent
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
- Fonts are the system stack; the dashboard loads nothing from the network.

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
