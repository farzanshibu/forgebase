import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { ACCENTS } from "./theme";

/** WCAG AA for normal-size text. */
const AA = 4.5;

const css = readFileSync(resolve(process.cwd(), "src/index.css"), "utf8");

/** `--name: #hex;` declarations of the first rule whose selector is exactly `selector`. */
function tokens(selector: string): Record<string, string> {
  const esc = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const body = new RegExp(`(?:^|\\n)${esc}\\s*\\{([^}]*)\\}`).exec(css)?.[1];
  if (body === undefined) throw new Error(`no rule for ${selector}`);
  const out: Record<string, string> = {};
  for (const m of body.matchAll(/--([a-z0-9-]+):\s*(#[0-9a-f]{6})\s*;/gi)) out[m[1]!] = m[2]!.toLowerCase();
  return out;
}

const rgb = (hex: string) => [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16)) as [number, number, number];
const channel = (c: number) => {
  const s = c / 255;
  return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
};
const luminance = ([r, g, b]: number[]) => 0.2126 * channel(r!) + 0.7152 * channel(g!) + 0.0722 * channel(b!);
const ratio = (a: number[], b: number[]) => {
  const [x, y] = [luminance(a), luminance(b)];
  return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
};
/** `pct` of `fg` over `bg`, as a tinted badge or alert background is drawn. */
const tint = (fg: number[], bg: number[], pct: number) => fg.map((v, i) => Math.round(v * pct + bg[i]! * (1 - pct)));

const light = tokens(":root");
const dark = { ...light, ...tokens(".dark") };
const MODES = { light, dark } as const;

function palette(mode: keyof typeof MODES, accent: string) {
  const sel = mode === "light" ? `[data-accent="${accent}"]` : `.dark[data-accent="${accent}"], .dark [data-accent="${accent}"]`;
  return { ...MODES[mode], ...tokens(sel) };
}

describe("theme contrast (WCAG AA, 4.5:1)", () => {
  for (const mode of ["light", "dark"] as const) {
    describe(mode, () => {
      const p = MODES[mode];
      const surfaces = ["background", "card", "muted", "popover"] as const;

      it("body and muted text on every surface", () => {
        for (const s of surfaces) {
          expect(ratio(rgb(p.foreground!), rgb(p[s]!)), `foreground on ${s}`).toBeGreaterThanOrEqual(AA);
          expect(ratio(rgb(p["muted-foreground"]!), rgb(p[s]!)), `muted-foreground on ${s}`).toBeGreaterThanOrEqual(AA);
        }
      });

      it("status colours as text on surfaces and on their own tint", () => {
        for (const t of ["success", "warning", "info", "destructive"] as const) {
          const c = rgb(p[t]!);
          for (const s of surfaces) expect(ratio(c, rgb(p[s]!)), `${t} on ${s}`).toBeGreaterThanOrEqual(AA);
          expect(ratio(c, tint(c, rgb(p.card!), 0.15)), `${t} on its 15% tint`).toBeGreaterThanOrEqual(AA);
        }
      });

      for (const a of ACCENTS) {
        it(`accent ${a.id}: label on the fill, and as text on surfaces and its tint`, () => {
          const q = palette(mode, a.id);
          const primary = rgb(q.primary!);
          expect(ratio(rgb(q["primary-foreground"]!), primary), "primary-foreground on primary").toBeGreaterThanOrEqual(AA);
          for (const s of surfaces) expect(ratio(primary, rgb(q[s]!)), `primary on ${s}`).toBeGreaterThanOrEqual(AA);
          expect(ratio(primary, tint(primary, rgb(q.card!), 0.15)), "primary on its 15% tint").toBeGreaterThanOrEqual(AA);
          expect(ratio(rgb(q.foreground!), tint(primary, rgb(q.background!), 0.15)), "foreground on primary tint").toBeGreaterThanOrEqual(AA);
        });
      }
    });
  }

  it("every accent preset has a light and a dark rule", () => {
    for (const a of ACCENTS) {
      expect(palette("light", a.id).primary).toMatch(/^#/);
      expect(tokens(`.dark[data-accent="${a.id}"], .dark [data-accent="${a.id}"]`).primary).toMatch(/^#/);
    }
  });
});
