import { useEffect, useState } from "react";
import type { QueueSubscription } from "../../api";
import { CopyButton } from "../../components/ui";
import { SecretOnce } from "../identity/shared";
import { Code } from "../jobs/ops";
import { clearSecret, listenCommand, peekSecret, signedCurl, verifySnippet } from "./webhook";

/** A snippet with its own copy button, so the receiver's code can be pasted straight in. */
function Snippet({ title, text }: { title: string; text: string }) {
  return (
    <div className="mt-3">
      <div className="mb-1 flex items-center justify-between gap-2">
        <span className="text-xs font-medium text-foreground">{title}</span>
        <CopyButton text={text} label={`Copy ${title}`} />
      </div>
      <Code label={title}>{text}</Code>
    </div>
  );
}

/**
 * The signing secret of a webhook subscription that was just created, shown once at the top of its page (the rotate-secret pattern).
 * It is read from the one-shot slot the New subscription sheet filled and cleared at once: this component's state is the only copy
 * left, so a reload, a revisit or the back button never shows it again. Render it with `key={sub}`.
 */
export default function NewSecretNotice({ name, sub }: { name: string; sub?: QueueSubscription }) {
  const [secret, setSecret] = useState(() => peekSecret(name));
  useEffect(() => { clearSecret(name); }, [name]);
  if (!secret) return null;
  return (
    <SecretOnce
      label={`Signing secret for ${name}`}
      secret={secret}
      onDismiss={() => setSecret(undefined)}
      note={
        <>
          <p>Store it now: it won&apos;t be shown again. Your receiver needs it to check that every call is genuinely from ForgeBase. If you lose it, rotate the secret to get a new one.</p>
          <Snippet title="Verify in Node" text={verifySnippet()} />
          <Snippet title="Try your receiver with curl" text={signedCurl(sub?.push?.endpoint ?? "https://example.com/hooks/forgebase")} />
          {sub && <Snippet title="Test against localhost" text={listenCommand(sub.topic, sub.filter)} />}
        </>
      }
    />
  );
}
