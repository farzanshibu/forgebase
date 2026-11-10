import { useState } from "react";
import { useNavigate } from "react-router-dom";
import type { FunctionLanguage, InvokeAccess } from "../../api";
import { Button, Panel } from "../../components/ui";
import { Sel } from "../../components/controls";
import { Input } from "panelui-web/input";
import CapabilitySwitches from "./CapabilitySwitches";
import DeployForm from "./DeployForm";
import SourceEditor from "./SourceEditor";
import { TEMPLATES, template, type TemplateId } from "./templates";

type Choice = FunctionLanguage | "wasm";
const NAME = /^[a-z][a-z0-9_-]{0,47}$/;

/** New function: name, template, language and capabilities, then the editor; or a WebAssembly upload. */
export default function NewFunction() {
  const nav = useNavigate();
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [access, setAccess] = useState<InvokeAccess>("permission");
  const [tpl, setTpl] = useState<TemplateId>("http");
  const [lang, setLang] = useState<Choice>("typescript");
  const [caps, setCaps] = useState<string[]>(template("http").capabilities);
  const [editing, setEditing] = useState(false);
  const [problem, setProblem] = useState("");
  const open = (n: string) => nav(`/functions/f/${encodeURIComponent(n)}`);

  function start(e: React.FormEvent) {
    e.preventDefault();
    if (!NAME.test(name)) return setProblem("Name: lower-case letters, digits, _ or -, starting with a letter");
    setProblem("");
    setEditing(true);
  }

  const languageSel = (
    <label className="text-sm">Language
      <Sel aria-label="Language" className="mt-1" value={lang} onChange={(v) => setLang(v)} disabled={editing}
        options={[{ value: "typescript", label: "TypeScript" }, { value: "javascript", label: "JavaScript" }, { value: "wasm", label: "WebAssembly (upload a .wasm)" }]} />
    </label>
  );

  if (lang === "wasm") {
    return (
      <div className="space-y-4">
        <div className="max-w-xs">{languageSel}</div>
        <DeployForm onDone={(d) => open(d.function.name)} />
      </div>
    );
  }

  const t = template(tpl);
  return (
    <div className="space-y-4">
      <Panel title="New function">
        <form aria-label="New function" onSubmit={start} className="grid gap-3 md:grid-cols-4">
          <label className="text-sm md:col-span-2">Name
            <Input size="sm" className="mt-1 w-full" value={name} onChange={(e) => setName(e.target.value)} readOnly={editing} required title="Lower-case letters, digits, _ or -, starting with a letter" />
          </label>
          <label className="text-sm md:col-span-2">Description<Input size="sm" className="mt-1 w-full" value={description} onChange={(e) => setDescription(e.target.value)} readOnly={editing} /></label>
          <label className="text-sm">Template
            <Sel aria-label="Template" className="mt-1" value={tpl} disabled={editing}
              onChange={(v) => { setTpl(v); setCaps(template(v).capabilities); }}
              options={TEMPLATES.map((x) => ({ value: x.id, label: x.label }))} />
          </label>
          {languageSel}
          <label className="text-sm">HTTP access
            <Sel aria-label="HTTP access" className="mt-1" value={access} onChange={(v) => setAccess(v as InvokeAccess)} disabled={editing}
              options={[{ value: "permission", label: "permission" }, { value: "authenticated", label: "authenticated" }, { value: "public", label: "public" }]} />
          </label>
          <p className="self-end pb-2 text-sm text-muted-foreground">{t.description}</p>
          {!editing && <div className="md:col-span-4"><CapabilitySwitches value={caps} onChange={setCaps} /></div>}
          {!editing && <div className="md:col-span-4"><Button tone="primary" type="submit">Open editor</Button></div>}
        </form>
        {problem && <p role="alert" className="mt-3 text-sm text-destructive">{problem}</p>}
      </Panel>
      {editing && (
        <Panel title={`Editor: ${name}`}>
          <div className="mb-3"><Button onClick={() => setEditing(false)}>Back to settings</Button></div>
          <SourceEditor
            key={`${tpl}:${lang}`}
            name={name}
            language={lang}
            initialSource={t.source[lang]}
            storageKey={`new:${name}`}
            capabilities={caps}
            config={{ hooks: t.hooks }}
            description={description}
            access={access}
            onDeployed={(d) => open(d.function.name)}
          />
        </Panel>
      )}
    </div>
  );
}
