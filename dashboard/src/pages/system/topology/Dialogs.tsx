/**
 * The dialogs of the Database topology screen (FB-176): add a replica (here, on another host with a join token, or one
 * you run), add a shard, promote with its data-loss check, rebuild, remove, set a weight, drain.
 * Each says what will happen in plain words; removing and promoting ask for the server's name (dashboard-ux.md rule 5).
 */
import { Input } from "panelui-web/input";
import { useState, type FormEvent } from "react";
import {
  addNode, addShard, issueJoinToken, promoteCheck, promoteNode, rebuildNode, removeNode, removeShard, setNodeWeight, updateShard,
  type JoinToken, type TopologyNode, type TopologySet,
} from "../../../api";
import { Check, Sel } from "../../../components/controls";
import { ErrorPanel } from "../../../components/ErrorPanel";
import { FormField } from "../../../components/FormField";
import { HelpTip } from "../../../components/HelpTip";
import { Button, CopyButton } from "../../../components/ui";
import { formatBytes } from "../../../format";
import { useLoad } from "../../../hooks";
import { toaster } from "../../../toaster";
import { ActionDialog, DialogShell } from "./ui";
import { validName, validUrl } from "./util";

const done = (title: string, description?: string) => toaster.add({ title, description, type: "success" });

type Mode = "local" | "remote" | "external";

const MODES: { id: Mode; label: string; text: string }[] = [
  { id: "local", label: "On this server", text: "ForgeBase copies the database to a new replica next to the primary. Nothing to install." },
  { id: "remote", label: "On another host", text: "You get a one-time command to run on the other machine, which then joins by itself." },
  { id: "external", label: "A server I run", text: "Point ForgeBase at a PostgreSQL replica you already run, by its address." },
];

/** Add a replica: pick where it runs, name it, add it. */
export function AddReplicaDialog({ sets, initialSet, onClose, onChanged }: { sets: TopologySet[]; initialSet: string; onClose: () => void; onChanged: () => void }) {
  const [mode, setMode] = useState<Mode>("local");
  const [set, setSet] = useState(initialSet);
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const [weight, setWeight] = useState("1");
  const [touched, setTouched] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>();
  const [token, setToken] = useState<JoinToken>();

  const nameErr = mode === "remote" ? undefined : validName(name);
  const urlErr = mode === "external" ? validUrl(url) : undefined;
  const weightN = Number(weight);
  const weightErr = mode === "remote" || (Number.isInteger(weightN) && weightN >= 1 && weightN <= 1000) ? undefined : "Use a whole number from 1 to 1000.";
  const invalid = Boolean(nameErr || urlErr || weightErr);

  async function submit(e: FormEvent) {
    e.preventDefault();
    setTouched(true);
    if (invalid || busy) return;
    setBusy(true);
    setError(undefined);
    try {
      if (mode === "remote") {
        setToken(await issueJoinToken(set));
      } else {
        await addNode({ set, name, kind: mode, ...(mode === "external" ? { url: url.trim() } : {}), weight: weightN });
        done(mode === "local" ? `Adding replica ${name}` : `Replica ${name} added`, mode === "local" ? "ForgeBase is copying the database. Follow it in Operations." : undefined);
        onChanged();
        onClose();
      }
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }

  if (token) {
    return (
      <DialogShell title="Join command" onClose={() => { onChanged(); onClose(); }} footer={<Button tone="primary" onClick={() => { onChanged(); onClose(); }}>Done</Button>}>
        <p role="note" className="rounded border border-warning/40 bg-warning/10 p-2 text-warning">Copy this command now. The token in it is shown once and stops working at {new Date(token.expires_at).toLocaleTimeString()} (15 minutes).</p>
        <pre aria-label="Join command" className="overflow-x-auto rounded-md bg-background p-3 font-mono text-xs">{token.command}</pre>
        <CopyButton text={token.command} label="Copy command" />
        <p className="text-xs text-muted-foreground">Run it on the other host (it needs the <code>forgebase</code> program and PostgreSQL). The replica appears in {set === "control" ? "Control" : "the set"} when it connects.</p>
      </DialogShell>
    );
  }

  return (
    <DialogShell
      title="Add a replica"
      description={<>A <HelpTip term="replica">read replica</HelpTip> is a copy of your database that answers reads and can take over if the primary fails. Your data is never changed by adding one.</>}
      onClose={() => { if (!busy) onClose(); }}
      footer={
        <>
          <Button onClick={onClose} disabled={busy}>Cancel</Button>
          <Button tone="primary" type="submit" form="add-replica" disabled={busy}>{busy ? "Working…" : mode === "remote" ? "Create join command" : "Add replica"}</Button>
        </>
      }
    >
      <form id="add-replica" aria-label="Add a replica" onSubmit={(e) => void submit(e)} className="space-y-3" noValidate>
        <div role="group" aria-label="Where the replica runs" className="grid gap-2">
          {MODES.map((m) => (
            <button key={m.id} type="button" aria-pressed={mode === m.id} onClick={() => setMode(m.id)} className={`rounded-lg border p-2 text-left ${mode === m.id ? "border-primary bg-primary/5" : "border-border hover:bg-muted/50"}`}>
              <span className="block font-medium">{m.label}</span>
              <span className="block text-xs text-muted-foreground">{m.text}</span>
            </button>
          ))}
        </div>
        {sets.length > 1 && (
          <FormField label="Add it to" term="topology_set">{(p) => <Sel {...p} size="md" className="w-full" value={set} onChange={setSet} options={sets.map((s) => ({ value: s.id, label: s.name }))} />}</FormField>
        )}
        {mode !== "remote" && (
          <FormField label="Name" help="Lower-case letters, digits, dashes and underscores, up to 32." error={touched ? nameErr : undefined} required>
            {(p) => <Input {...p} className="w-full" value={name} onChange={(e) => setName(e.target.value.trim())} autoComplete="off" spellCheck={false} />}
          </FormField>
        )}
        {mode === "external" && (
          <FormField label="Address" help="Stored sealed and never shown again. ForgeBase checks the connection first." error={touched ? urlErr : undefined} required>
            {(p) => <Input {...p} type="password" className="w-full font-mono" value={url} onChange={(e) => setUrl(e.target.value)} autoComplete="off" spellCheck={false} placeholder="postgres://user:password@host:5432/forgebase" />}
          </FormField>
        )}
        {mode !== "remote" && (
          <FormField label="Share of reads" help="1 is the normal share; 2 sends it twice as many reads as a replica with 1." error={touched ? weightErr : undefined}>
            {(p) => <Input {...p} className="w-24" inputMode="numeric" value={weight} onChange={(e) => setWeight(e.target.value)} />}
          </FormField>
        )}
        <ErrorPanel error={error} />
      </form>
    </DialogShell>
  );
}

/** Add a shard: a second database server that takes new tenants. */
export function AddShardDialog({ onClose, onChanged }: { onClose: () => void; onChanged: () => void }) {
  const [name, setName] = useState("");
  const [nodeName, setNodeName] = useState("");
  const [url, setUrl] = useState("");
  const [touched, setTouched] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>();
  const nameErr = validName(name);
  const nodeErr = validName(nodeName);
  const urlErr = validUrl(url);

  async function submit(e: FormEvent) {
    e.preventDefault();
    setTouched(true);
    if (nameErr || nodeErr || urlErr || busy) return;
    setBusy(true);
    setError(undefined);
    try {
      await addShard({ name, node: { name: nodeName, kind: "external", url: url.trim() } });
      done(`Adding shard ${name}`, "ForgeBase is preparing it. Follow it in Operations. It takes new tenants once it is active.");
      onChanged();
      onClose();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }

  return (
    <DialogShell
      title="Add a shard"
      description={<>A <HelpTip term="shard">shard</HelpTip> is another database server that holds some of your tenants, so one server does not have to hold them all. New tenants can go to it; existing tenants stay where they are until you move them. Nothing changes until it is active.</>}
      onClose={() => { if (!busy) onClose(); }}
      footer={<><Button onClick={onClose} disabled={busy}>Cancel</Button><Button tone="primary" type="submit" form="add-shard" disabled={busy}>{busy ? "Working…" : "Add shard"}</Button></>}
    >
      <form id="add-shard" aria-label="Add a shard" onSubmit={(e) => void submit(e)} className="space-y-3" noValidate>
        <FormField label="Shard name" help="For example eu-1. Lower-case letters, digits, dashes and underscores." error={touched ? nameErr : undefined} required>
          {(p) => <Input {...p} className="w-full" value={name} onChange={(e) => setName(e.target.value.trim())} autoComplete="off" spellCheck={false} />}
        </FormField>
        <FormField label="Primary server name" help="A name for the shard's own server." error={touched ? nodeErr : undefined} required>
          {(p) => <Input {...p} className="w-full" value={nodeName} onChange={(e) => setNodeName(e.target.value.trim())} autoComplete="off" spellCheck={false} />}
        </FormField>
        <FormField label="Server address" help="A PostgreSQL server you run, with max_prepared_transactions above 0. Stored sealed and never shown again." error={touched ? urlErr : undefined} required>
          {(p) => <Input {...p} type="password" className="w-full font-mono" value={url} onChange={(e) => setUrl(e.target.value)} autoComplete="off" spellCheck={false} placeholder="postgres://user:password@host:5432/forgebase" />}
        </FormField>
        <ErrorPanel error={error} />
      </form>
    </DialogShell>
  );
}

/** Promote a replica: what happens, the data-loss estimate when the primary is down, then the typed name. */
export function PromoteDialog({ node, onClose, onChanged }: { node: TopologyNode; onClose: () => void; onChanged: () => void }) {
  const check = useLoad((s) => promoteCheck(node.id, s), `promote:${node.id}`);
  const [accept, setAccept] = useState(false);
  const down = check.data ? !check.data.primary_reachable : false;
  return (
    <ActionDialog
      title={`Promote ${node.name} to primary?`}
      consequence={<>
        <HelpTip term="promote">Promoting</HelpTip> makes <strong>{node.name}</strong> the server that takes writes. The current primary stops being the primary and must be rebuilt as a replica before it rejoins. Writes pause for a few seconds while it switches.
      </>}
      confirmLabel="Promote to primary"
      confirmText={node.name}
      tone="danger"
      canConfirm={Boolean(check.data) && (!down || accept)}
      onConfirm={async () => {
        await promoteNode(node.id, node.name, down && accept);
        done(`Promoting ${node.name}`, "Follow it in Operations.");
        onChanged();
      }}
      onClose={onClose}
    >
      {!check.data && !check.error && <p role="status" className="text-muted-foreground">Checking the servers…</p>}
      <ErrorPanel error={check.error} onRetry={check.reload} />
      {check.data && !down && (
        <p role="status" className="rounded-md border border-success/40 bg-success/10 p-2">
          The primary is reachable. {typeof check.data.behind_bytes === "number" ? (check.data.behind_bytes === 0 ? `${node.name} has every change.` : `${node.name} is ${formatBytes(check.data.behind_bytes)} behind; ForgeBase lets it catch up first, so nothing is lost.`) : `ForgeBase lets ${node.name} catch up first, so nothing is lost.`}
        </p>
      )}
      {check.data && down && (
        <div role="alert" className="space-y-2 rounded-md border border-destructive/50 bg-destructive/10 p-3">
          <p className="font-medium text-destructive">The old primary is down</p>
          <p>ForgeBase cannot ask it for its last changes. {typeof check.data.behind_bytes === "number" ? `Up to ${formatBytes(check.data.behind_bytes)} of the newest changes may be lost.` : "Some of the newest changes may be lost; the amount is not known."}</p>
          <Check checked={accept} onChange={setAccept} label="I accept that the newest changes may be lost" />
        </div>
      )}
    </ActionDialog>
  );
}

export function RebuildDialog({ node, onClose, onChanged }: { node: TopologyNode; onClose: () => void; onChanged: () => void }) {
  return (
    <ActionDialog
      title={`Rebuild ${node.name}?`}
      consequence={<>ForgeBase copies the database to <strong>{node.name}</strong> again. It does not answer reads until the copy is done. The primary and your data are not touched.</>}
      confirmLabel="Rebuild replica"
      tone="primary"
      onConfirm={async () => { await rebuildNode(node.id); done(`Rebuilding ${node.name}`, "Follow it in Operations."); onChanged(); }}
      onClose={onClose}
    />
  );
}

export function RemoveReplicaDialog({ node, onClose, onChanged }: { node: TopologyNode; onClose: () => void; onChanged: () => void }) {
  return (
    <ActionDialog
      title={`Remove replica ${node.name}?`}
      consequence={<>ForgeBase stops sending reads to <strong>{node.name}</strong> and forgets it{node.kind === "local" ? ", and deletes its copy of the data on this server" : ". The server and its data stay where they are"}. Your primary data is not touched.</>}
      confirmLabel="Remove replica"
      confirmText={node.name}
      onConfirm={async () => { await removeNode(node.id, node.name); done(`Replica ${node.name} removed`); onChanged(); }}
      onClose={onClose}
    />
  );
}

export function RemoveShardDialog({ set, onClose, onChanged }: { set: TopologySet; onClose: () => void; onChanged: () => void }) {
  return (
    <ActionDialog
      title={`Remove shard ${set.name}?`}
      consequence={<>ForgeBase forgets <strong>{set.name}</strong> and its servers. A shard that still holds tenants cannot be removed: move or drain them first. The servers and their data stay where they are.</>}
      confirmLabel="Remove shard"
      confirmText={set.name}
      onConfirm={async () => { await removeShard(set.id, set.name); done(`Shard ${set.name} removed`); onChanged(); }}
      onClose={onClose}
    />
  );
}

/** Drain stops new tenants; resume lets the shard take them again. Neither moves data. */
export function DrainDialog({ set, resume, onClose, onChanged }: { set: TopologySet; resume: boolean; onClose: () => void; onChanged: () => void }) {
  return (
    <ActionDialog
      title={resume ? `Let ${set.name} take new tenants again?` : `Drain ${set.name}?`}
      consequence={resume ? <>New tenants may be placed on <strong>{set.name}</strong> again.</> : <>New tenants are no longer placed on <strong>{set.name}</strong>. The {set.tenants} tenant{set.tenants === 1 ? "" : "s"} already there stay and keep working. Move them away with Rebalance when you are ready to remove the shard.</>}
      confirmLabel={resume ? "Take new tenants" : "Drain shard"}
      tone="primary"
      onConfirm={async () => { await updateShard(set.id, { state: resume ? "active" : "draining" }); done(resume ? `${set.name} takes new tenants again` : `${set.name} is draining`); onChanged(); }}
      onClose={onClose}
    />
  );
}

/** Set a replica's share of reads, or a shard's share of new tenants. */
export function WeightDialog({ target, onClose, onChanged }: { target: { kind: "node"; node: TopologyNode } | { kind: "set"; set: TopologySet }; onClose: () => void; onChanged: () => void }) {
  const current = target.kind === "node" ? target.node.weight : target.set.weight;
  const name = target.kind === "node" ? target.node.name : target.set.name;
  const [value, setValue] = useState(String(current));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>();
  const n = Number(value);
  const err = Number.isInteger(n) && n >= 1 && n <= 1000 ? undefined : "Use a whole number from 1 to 1000.";

  async function submit(e: FormEvent) {
    e.preventDefault();
    if (err || busy) return;
    setBusy(true);
    setError(undefined);
    try {
      if (target.kind === "node") await setNodeWeight(target.node.id, n);
      else await updateShard(target.set.id, { weight: n });
      done(`${name} set to ${n}`);
      onChanged();
      onClose();
    } catch (e2) {
      setError(e2);
    } finally {
      setBusy(false);
    }
  }
  return (
    <DialogShell
      title={target.kind === "node" ? `Read share of ${name}` : `Capacity share of ${name}`}
      description={target.kind === "node" ? "How many reads this replica gets compared with the others. 2 means twice as many as a replica set to 1." : "How many new tenants this shard gets compared with the others. 2 means twice as many as a shard set to 1."}
      onClose={() => { if (!busy) onClose(); }}
      footer={<><Button onClick={onClose} disabled={busy}>Cancel</Button><Button tone="primary" type="submit" form="set-weight" disabled={Boolean(err) || busy}>Save</Button></>}
    >
      <form id="set-weight" aria-label="Set share" onSubmit={(e) => void submit(e)} className="space-y-3" noValidate>
        <FormField label="Share" error={err}>{(p) => <Input {...p} className="w-24" inputMode="numeric" value={value} onChange={(e) => setValue(e.target.value)} />}</FormField>
        <ErrorPanel error={error} />
      </form>
    </DialogShell>
  );
}
