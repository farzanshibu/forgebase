import { useState } from "react";
import { exportRecords, importRecords } from "../../api";
import type { CollectionDef, ExportFormat, ImportResult } from "../../api";
import { Download04Icon, FileExportIcon, FileImportIcon, Upload04Icon } from "@hugeicons/core-free-icons";
import { Button, ErrorView } from "../../components/ui";
import { FormField } from "../../components/FormField";
import { formatBytes } from "../../format";
import { Icon } from "./parts";
import { Input } from "panelui-web/input";
import { Sel } from "../../components/controls";

const FORMATS: ExportFormat[] = ["json", "ndjson", "csv"];
const FORMAT_WORDS: Record<ExportFormat, string> = { json: "JSON", ndjson: "NDJSON", csv: "CSV" };

export default function ImportExport({ collection }: { collection: CollectionDef }) {
  const name = collection.name;
  const [format, setFormat] = useState<ExportFormat>("json");
  const [filter, setFilter] = useState("");
  const [file, setFile] = useState<File>();
  const [problem, setProblem] = useState<unknown>();
  const [result, setResult] = useState<ImportResult>();
  const [busy, setBusy] = useState(false);

  async function doExport() {
    setBusy(true);
    setProblem(undefined);
    try {
      const text = await exportRecords(name, format, filter || undefined);
      const url = URL.createObjectURL(new Blob([text], { type: "text/plain" }));
      const a = document.createElement("a");
      a.href = url;
      a.download = `${name}.${format}`;
      a.click();
      URL.revokeObjectURL(url);
    } catch (e) {
      setProblem(e);
    } finally {
      setBusy(false);
    }
  }

  async function doImport() {
    if (!file) return;
    setBusy(true);
    setProblem(undefined);
    setResult(undefined);
    try {
      const ext = file.name.split(".").pop()?.toLowerCase();
      const fmt: ExportFormat = ext === "csv" ? "csv" : ext === "ndjson" ? "ndjson" : format;
      setResult(await importRecords(name, fmt, await file.text()));
    } catch (e) {
      setProblem(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="max-w-5xl space-y-4">
      <ErrorView error={problem} />
      <div className="grid gap-4 lg:grid-cols-2">
        <section aria-label="Export" className="flex flex-col rounded-xl border border-border bg-card p-5">
          <h2 className="flex items-center gap-2 text-sm font-medium"><Icon icon={FileExportIcon} className="text-muted-foreground" />Export</h2>
          <p className="mt-1 mb-4 text-xs text-muted-foreground">Download the records of {name} as a file. Add a filter to export only some of them.</p>
          <div className="grid flex-1 content-start gap-4 text-sm sm:grid-cols-[9rem_1fr]">
            <FormField label="Format">
              {(p) => <Sel {...p} aria-label="Format" className="w-full" value={format} onChange={(v) => setFormat(v as ExportFormat)} options={FORMATS.map((f) => ({ value: f, label: FORMAT_WORDS[f] }))} />}
            </FormField>
            <FormField label="Filter" help="Optional. Same syntax as the Data tab.">
              {(p) => <Input {...p} size="sm" className="font-mono" placeholder='status = "live"' value={filter} onChange={(e) => setFilter(e.target.value)} />}
            </FormField>
          </div>
          <div className="mt-4 flex justify-end">
            <Button disabled={busy} onClick={() => void doExport()}>
              <Icon icon={Download04Icon} size={14} />
              Export
            </Button>
          </div>
        </section>
        <section aria-label="Import" className="flex flex-col rounded-xl border border-border bg-card p-5">
          <h2 className="flex items-center gap-2 text-sm font-medium"><Icon icon={FileImportIcon} className="text-muted-foreground" />Import</h2>
          <p className="mt-1 mb-4 text-xs text-muted-foreground">JSON array, NDJSON or CSV. Stored in batches of 200; the import stops at the first failing record and earlier batches stay committed.</p>
          <div className="flex-1 text-sm">
            <Input size="sm" aria-label="Import file" type="file" accept=".json,.ndjson,.csv" className="w-full" onChange={(e) => setFile(e.target.files?.[0])} />
            {file && <p className="mt-2 text-xs text-muted-foreground">{file.name} · {formatBytes(file.size)}</p>}
            {result && <p role="status" className="mt-3 text-sm text-success">Imported {result.imported} records ({result.format}).</p>}
          </div>
          <div className="mt-4 flex justify-end">
            <Button tone="primary" disabled={busy || !file} onClick={() => void doImport()}>
              <Icon icon={Upload04Icon} size={14} />
              Import
            </Button>
          </div>
        </section>
      </div>
    </div>
  );
}
