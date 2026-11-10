/**
 * Connect your app: the server URL, how to get an API key, and an install plus first-query snippet against the newest
 * collection (dashboard-ux.md, "Overview is the starting point"; audit A-02). Keys are always `<YOUR_API_KEY>`.
 *
 * Usage:
 *   <ConnectApp collection={newestCollection(list)?.name} />   // no collection yet: the snippet reads `posts`
 */
import { useId } from "react";
import { Card } from "panelui-web/card";
import { CopyButton } from "../../components/ui";
import { UseFromCode } from "../../components/UseFromCode";
import { collection as collectionSnippet, serverUrl, type Snippet } from "../../snippets";
import { LinkButton } from "./LinkButton";

/** The name the snippet uses while the project has no collection. */
export const PLACEHOLDER_COLLECTION = "posts";

/** Install, connect and list the first page of a collection, in JavaScript, Rust and curl. */
export function firstQuerySnippet(url: string, name: string): Snippet {
  const list = collectionSnippet(url, name);
  return {
    js: `// Install: npm install @forgebase/sdk\n${list.js}`,
    rust: `// Install: cargo add forgebase-sdk tokio serde_json\n${list.rust}`,
    curl: `# List the newest records of ${name}\n${list.curl}`,
  };
}

export function ConnectApp({ collection }: { collection?: string }) {
  const headingId = useId();
  const url = serverUrl();
  const name = collection ?? PLACEHOLDER_COLLECTION;
  return (
    <Card as="section" aria-labelledby={headingId} className="min-w-0 rounded-xl">
      <Card.Header>
        <Card.Title as="h2" id={headingId} className="text-base font-semibold">Connect your app</Card.Title>
        <Card.Description>Call this server from your code with the JavaScript or Rust SDK, or plain HTTP.</Card.Description>
      </Card.Header>
      <Card.Content className="space-y-5">
        <div>
          <p className="text-sm font-medium" id={`${headingId}-url`}>Server URL</p>
          <div className="mt-1.5 flex flex-wrap items-center gap-2">
            <code aria-labelledby={`${headingId}-url`} className="min-w-0 flex-1 truncate rounded-md bg-muted/60 px-2.5 py-1.5 font-mono text-sm">{url}</code>
            <CopyButton text={url} label="Copy server URL" />
          </div>
        </div>

        <div>
          <p className="text-sm font-medium">API key</p>
          <p className="mt-1 text-xs text-muted-foreground">
            Your app sends a key with every request. Create one that can read {collection ? <code className="font-mono">{name}</code> : "your collection"}, and keep it on your server, never in a browser app.
          </p>
          <LinkButton to="/auth/keys?new=1" className="mt-2">Create an API key</LinkButton>
        </div>

        <div>
          <UseFromCode title="First query" snippet={firstQuerySnippet(url, name)} />
          {!collection && (
            <p className="mt-2 text-xs text-muted-foreground">
              You have no collection yet, so the snippet reads <code className="font-mono">{PLACEHOLDER_COLLECTION}</code>. Create a collection and its name appears here.
            </p>
          )}
        </div>
      </Card.Content>
    </Card>
  );
}

export default ConnectApp;
