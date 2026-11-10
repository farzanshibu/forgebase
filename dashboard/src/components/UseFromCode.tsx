/**
 * UseFromCode: copyable JavaScript SDK, Rust SDK and curl snippets for one resource (dashboard-ux.md rule 3,
 * audit A-03). Filled in with the server URL and the resource name; the key is always `<YOUR_API_KEY>`.
 *
 * Usage:
 *   <UseFromCode kind="collection" name="posts" />                  // inline panel with three tabs
 *   <UseFromCodeButton kind="bucket" name="avatars" />              // header button that opens it in a side sheet
 *   <UseFromCode snippet={snippets.vectorCollection(serverUrl(), "docs", "embedding", 1536)} />  // custom generator args
 *
 * Kinds: collection, record, bucket, object ("bucket/key"), function, topic, subscription, jobKind, kvNamespace,
 * vectorCollection, graph. New kinds go in src/snippets.ts.
 */
import { Sheet } from "panelui-web/sheet";
import { Tabs } from "panelui-web/tabs";
import { useState } from "react";
import { serverUrl, snippets, type Snippet, type SnippetKind } from "../snippets";
import { Button, CopyButton } from "./Button";

type Source = { kind: SnippetKind; name: string; snippet?: undefined } | { snippet: Snippet; kind?: undefined; name?: undefined };

export type UseFromCodeProps = Source & {
  /** Heading above the tabs; default "Use from code". */
  title?: string;
  className?: string;
};

const LANGS = [
  { id: "js", label: "JavaScript", hint: "npm install @forgebase/sdk" },
  { id: "rust", label: "Rust", hint: "cargo add forgebase-sdk tokio serde_json" },
  { id: "curl", label: "curl", hint: "Any shell" },
] as const;

function resolve(p: Source): Snippet {
  return p.snippet ?? snippets[p.kind](serverUrl(), p.name);
}

export function UseFromCode(props: UseFromCodeProps) {
  const { title = "Use from code", className = "" } = props;
  const code = resolve(props);
  const [lang, setLang] = useState<string>("js");
  return (
    <section aria-label={title} className={`min-w-0 space-y-2 ${className}`.trim()}>
      <div>
        <h2 className="text-sm font-medium">{title}</h2>
        <p className="text-xs text-muted-foreground">
          Replace <code className="font-mono">&lt;YOUR_API_KEY&gt;</code> with a key from Authentication › API keys &amp; tokens. Keep keys on the server, never in a browser app.
        </p>
      </div>
      <Tabs variant="underline" value={lang} onValueChange={(v) => setLang(String(v))}>
        <Tabs.List aria-label="Language">
          {LANGS.map((l) => (
            <Tabs.Trigger key={l.id} value={l.id}>
              {l.label}
            </Tabs.Trigger>
          ))}
        </Tabs.List>
        {LANGS.map((l) => (
          <Tabs.Content key={l.id} value={l.id} className="pt-2">
            <div className="flex items-center justify-between gap-2">
              <p className="font-mono text-xs text-muted-foreground">{l.hint}</p>
              <CopyButton text={code[l.id]} label={`Copy ${l.label} snippet`} />
            </div>
            <pre aria-label={`${l.label} snippet`} tabIndex={0} className="mt-2 max-h-96 overflow-auto rounded-lg bg-muted/60 p-3 font-mono text-xs leading-relaxed text-foreground">
              <code>{code[l.id]}</code>
            </pre>
          </Tabs.Content>
        ))}
      </Tabs>
    </section>
  );
}

/** A header button that opens UseFromCode in a side sheet. */
export function UseFromCodeButton(props: Source & { label?: string }) {
  const { label = "Use from code" } = props;
  const name = props.kind ? props.name : undefined;
  return (
    <Sheet side="right">
      <Sheet.Trigger render={<Button type="button" />}>{label}</Sheet.Trigger>
      <Sheet.Content size="auto" className="w-full max-w-2xl">
        <Sheet.Header title={label} description={name ? `Snippets for ${name}, filled in with this server's address.` : "Snippets filled in with this server's address."} />
        <Sheet.Body>{props.kind ? <UseFromCode kind={props.kind} name={props.name} title="Snippets" /> : <UseFromCode snippet={props.snippet!} title="Snippets" />}</Sheet.Body>
      </Sheet.Content>
    </Sheet>
  );
}

export default UseFromCode;
