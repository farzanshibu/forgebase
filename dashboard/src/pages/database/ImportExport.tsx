import { useState } from "react";
import { exportRecords, importRecords } from "../../api";
import type { CollectionDef, ExportFormat, ImportResult } from "../../api";
import { Button, ErrorView, Panel, inputCls } from "../../components/ui";

const FORMATS: ExportFormat[] = ["json", "ndjson", "csv"];

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
    <div className="max-w-2xl space-y-4">
      <ErrorView error={problem} />
      <Panel title="Export">
        <div className="flex flex-wrap items-end gap-3 text-sm">
          <label className="flex flex-col gap-1">Format
            <select className={inputCls} value={format} onChange={(e) => setFormat(e.target.value as ExportFormat)}>
              {FORMATS.map((f) => <option key={f}>{f}</option>)}
            </select>
          </label>
          <label className="flex flex-col gap-1">Filter (optional)
            <input className={`${inputCls} w-64 font-mono`} value={filter} onChange={(e) => setFilter(e.target.value)} />
          </label>
          <Button disabled={busy} onClick={() => void doExport()}>Export</Button>
        </div>
      </Panel>
      <Panel title="Import">
        <p className="mb-2 text-sm text-zinc-400">JSON array, NDJSON or CSV. Stored in batches of 200; the import stops at the first failing record and earlier batches stay committed.</p>
        <div className="flex flex-wrap items-center gap-3 text-sm">
          <input aria-label="Import file" type="file" accept=".json,.ndjson,.csv" onChange={(e) => setFile(e.target.files?.[0])} />
          <Button tone="primary" disabled={busy || !file} onClick={() => void doImport()}>Import</Button>
        </div>
        {result && <p role="status" className="mt-3 text-sm text-emerald-300">Imported {result.imported} records ({result.format}).</p>}
      </Panel>
    </div>
  );
}
