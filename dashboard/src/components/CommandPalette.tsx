/**
 * CommandPalette: Ctrl/Cmd+K (or the top bar's Search button) jumps to any studio, tab, resource or create action
 * (dashboard-ux.md rule 7, audit A-06). The Layout mounts it once; pages do nothing to appear in it.
 *
 * Sections:
 *   Go to      every visible studio and its tabs, from studios.ts
 *   Create     New collection, New bucket, New function, Create API key, New topic (studios.ts CREATE_ACTIONS)
 *   Resources  collections, buckets, functions, topics and vector collections, fetched when it opens, cached 30 s
 *
 * Usage (the Layout does this):
 *   <CommandPalette studios={visibleStudios(features)} />
 *
 * Arrow keys move, Enter goes, Escape closes. It is a dialog holding a combobox, so screen readers announce it.
 * A studio whose feature is off is neither listed nor queried. To add a resource kind, add a SOURCES entry.
 */
import { Add01Icon, Search01Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon, type IconSvgElement } from "@hugeicons/react";
import { Command } from "panelui-web/command";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";
import { CREATE_ACTIONS, GROUPS, studioHref, type Studio } from "../studios";
import { cachedResources, isMac, loadResources, paletteKeys, type PaletteResource } from "./paletteData";
import { resourceIcon, studioIcon } from "./studioIcons";

const glyph = (svg: IconSvgElement) => <HugeiconsIcon icon={svg} size={16} strokeWidth={1.8} aria-hidden="true" />;

export function CommandPalette({ studios }: { studios: Studio[] }) {
  const [open, setOpen] = useState(false);
  const [resources, setResources] = useState<PaletteResource[]>();
  const [search, setSearch] = useState("");
  const navigate = useNavigate();
  const paths = useMemo(() => studios.map((s) => s.path), [studios]);
  const pathKey = paths.join(",");

  // Ctrl+K and Cmd+K on every platform (PanelUI's own hotkey only knows one of them per platform).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setOpen((o) => !o);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  useEffect(() => {
    if (!open) return;
    const ctrl = new AbortController();
    void loadResources(pathKey.split(","), ctrl.signal).then((r) => !ctrl.signal.aborted && setResources(r));
    return () => ctrl.abort();
  }, [open, pathKey]);

  const change = useCallback((v: boolean) => {
    setOpen(v);
    if (!v) setSearch("");
    else setResources((r) => cachedResources() ?? r);
  }, []);
  const go = (to: string) => {
    change(false);
    navigate(to);
  };

  const groupLabel = (s: Studio) => GROUPS.find((g) => g.id === s.group)?.label ?? "";
  const creates = CREATE_ACTIONS.filter((a) => paths.includes(a.studio));
  return (
    <Command.Dialog
      open={open}
      onOpenChange={change}
      hotkey={null}
      label="Command palette"
      trigger={
        <button
          type="button"
          aria-keyshortcuts="Control+K Meta+K"
          className="inline-flex h-8 items-center gap-2 rounded-lg border border-border bg-background px-2.5 text-sm text-muted-foreground hover:bg-accent hover:text-foreground focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring"
        >
          <HugeiconsIcon icon={Search01Icon} size={16} strokeWidth={1.8} aria-hidden="true" />
          <span>Search</span>
          <kbd aria-hidden="true" className="hidden rounded border border-border bg-muted px-1.5 font-sans text-[0.6875rem] font-medium text-muted-foreground sm:inline-block">
            {paletteKeys().join(isMac() ? "" : "+")}
          </kbd>
        </button>
      }
    >
      <Command search={search} onSearchChange={setSearch} label="Results">
        <Command.Input placeholder="Search studios, resources and actions" />
        <Command.List>
          <Command.Empty>Nothing matches “{search}”. Try a studio name, such as Storage, or a resource name.</Command.Empty>
          <Command.Group heading="Go to">
            {studios.map((s) => (
              <Command.Item key={`go:${s.path}`} value={`go:${s.path}`} textValue={s.label} keywords={[s.title, groupLabel(s), s.summary].filter(Boolean)} subtitle={s.group ? groupLabel(s) : undefined} icon={glyph(studioIcon(s.path))} onSelect={() => go(studioHref(s))}>
                {s.label}
              </Command.Item>
            ))}
            {studios.flatMap((s) =>
              (s.tabs ?? [])
                .filter((t) => t.to !== studioHref(s))
                .map((t) => (
                  <Command.Item key={`tab:${t.to}`} value={`tab:${t.to}`} textValue={`${s.label} ${t.label}`} keywords={[s.title]} kind="Section" icon={glyph(studioIcon(s.path))} onSelect={() => go(t.to)}>
                    {s.label} › {t.label}
                  </Command.Item>
                )),
            )}
          </Command.Group>
          {creates.length > 0 && (
            <Command.Group heading="Create">
              {creates.map((a) => (
                <Command.Item key={`new:${a.to}`} value={`new:${a.to}`} textValue={a.label} keywords={a.keywords} kind="Action" icon={glyph(Add01Icon)} onSelect={() => go(a.to)}>
                  {a.label}
                </Command.Item>
              ))}
            </Command.Group>
          )}
          <Command.Group heading="Resources">
            {resources === undefined && open && <Command.Loading>Loading collections, buckets, functions and topics…</Command.Loading>}
            {(resources ?? []).map((r) => (
              <Command.Item key={`res:${r.to}`} value={`res:${r.to}`} textValue={r.name} keywords={[r.kind]} kind={r.kind} icon={glyph(resourceIcon(r.kind))} onSelect={() => go(r.to)}>
                {r.name}
              </Command.Item>
            ))}
          </Command.Group>
        </Command.List>
        <Command.Footer lead={<span className="text-xs text-muted-foreground">↑ ↓ to move, Enter to go, Esc to close</span>} />
      </Command>
    </Command.Dialog>
  );
}

export default CommandPalette;
