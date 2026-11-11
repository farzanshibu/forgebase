import { useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { listCollections } from "../../api";
import type { FunctionLanguage, InvokeAccess } from "../../api";
import { Panel } from "../../components/ui";
import { ErrorPanel } from "../../components/ErrorPanel";
import { FormField } from "../../components/FormField";
import { PageHeader } from "../../components/PageHeader";
import { SkeletonLines } from "../../components/Skeleton";
import { Sel } from "../../components/controls";
import { useLoad } from "../../hooks";
import { notify } from "../../toaster";
import { validate } from "../../validate";
import { Input } from "panelui-web/input";
import SourceEditor from "./SourceEditor";
import { TEMPLATES, hooksFor, template, type TemplateId } from "./templates";

const ACCESS: { value: InvokeAccess; label: string }[] = [
  { value: "permission", label: "Callers with the permission" },
  { value: "authenticated", label: "Any signed-in caller" },
  { value: "public", label: "Anyone (public)" },
];

/** A valid default function name from the template and the collection ("read-orders"). */
const suggest = (tpl: TemplateId, collection: string) =>
  [tpl, collection].filter(Boolean).join("-").toLowerCase().replace(/[^a-z0-9_-]+/g, "-").replace(/^[^a-z]+/, "").slice(0, 48) || "my-function";

/**
 * New function, on one page: pick a template, name it, and edit and deploy the code below.
 * A deploy toasts and opens the function's Test tab. WebAssembly modules are uploaded from the list ("Upload .wasm").
 */
export default function NewFunction() {
  const nav = useNavigate();
  const collections = useLoad(listCollections, "fn-new-collections");
  const [tpl, setTpl] = useState<TemplateId>("read");
  const [picked, setPicked] = useState("");
  const [typedName, setTypedName] = useState<string>();
  const [description, setDescription] = useState("");
  const [access, setAccess] = useState<InvokeAccess>("permission");
  const [lang, setLang] = useState<FunctionLanguage>("typescript");

  const t = template(tpl);
  const names = (collections.data ?? []).map((c) => c.name);
  // The newest collection is the likeliest one to build on; the person can pick another.
  const collection = t.needsCollection ? (picked || names[names.length - 1] || "") : "";
  const name = typedName ?? suggest(tpl, collection);
  const nameError = validate.functionName(name);
  const waiting = t.needsCollection && collections.loading;

  return (
    <div className="space-y-4">
      <PageHeader title="New function" description="Start from a template, edit the code, then deploy it. The Test tab opens next." docs="docs/adr/0017-functions.md"
        secondary={<Link to="/functions" className="text-sm text-primary hover:underline">Back to functions</Link>} />
      <Panel title="Template">
        <div role="group" aria-label="Templates" className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
          {TEMPLATES.map((x) => (
            <button key={x.id} type="button" aria-pressed={x.id === tpl} onClick={() => setTpl(x.id)}
              className={`rounded-lg border p-3 text-left text-sm hover:bg-muted/60 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring ${x.id === tpl ? "border-primary bg-muted/40" : "border-border"}`}>
              <span className="block font-medium">{x.label}</span>
              <span className="block text-xs text-muted-foreground">{x.description}</span>
            </button>
          ))}
        </div>
      </Panel>
      <Panel title="Settings">
        <form aria-label="Function settings" onSubmit={(e) => e.preventDefault()} className="grid gap-3 md:grid-cols-2">
          <FormField label="Name" required help="Lower-case letters, digits, dashes and underscores." error={typedName !== undefined ? nameError : undefined}>
            {(p) => <Input {...p} size="sm" className="w-full" value={name} onChange={(e) => setTypedName(e.target.value)} />}
          </FormField>
          <FormField label="Description">
            {(p) => <Input {...p} size="sm" className="w-full" value={description} onChange={(e) => setDescription(e.target.value)} />}
          </FormField>
          {t.needsCollection && (
            <FormField label="Collection" help={names.length ? "The function reads or writes this collection." : "No collections yet: type the name of the one you will create in Database."}>
              {(p) => names.length ? <Sel {...p} aria-label="Collection" value={collection} onChange={setPicked} options={names} /> : <Input {...p} size="sm" className="w-full" value={picked} onChange={(e) => setPicked(e.target.value)} />}
            </FormField>
          )}
          <FormField label="Language">
            {(p) => <Sel {...p} aria-label="Language" value={lang} onChange={(v) => setLang(v as FunctionLanguage)} options={[{ value: "typescript", label: "TypeScript" }, { value: "javascript", label: "JavaScript" }]} />}
          </FormField>
          <FormField label="HTTP access" help="Who may call this function over HTTP.">
            {(p) => <Sel {...p} aria-label="HTTP access" value={access} onChange={(v) => setAccess(v as InvokeAccess)} options={ACCESS} />}
          </FormField>
        </form>
        <div className="mt-2"><ErrorPanel error={collections.error} onRetry={collections.reload} /></div>
      </Panel>
      <Panel title={`Code: ${name || "new function"}`}>
        {waiting ? <SkeletonLines lines={4} label="Loading collections" /> : (
          <>
            <p className="mb-3 text-xs text-muted-foreground">Changing the template, collection or language starts the code again from the template.</p>
            <SourceEditor
              key={`${tpl}:${lang}:${collection}`}
              name={nameError ? "" : name}
              language={lang}
              initialSource={t.source(collection || "your_collection")[lang]}
              storageKey={`new:${name}`}
              capabilities={t.capabilities}
              config={{ hooks: hooksFor(t, collection) }}
              description={description}
              access={access}
              onDeployed={(d) => {
                notify.created(`Function ${d.function.name}`);
                nav(`/functions/f/${encodeURIComponent(d.function.name)}/test`);
              }}
            />
          </>
        )}
      </Panel>
    </div>
  );
}
