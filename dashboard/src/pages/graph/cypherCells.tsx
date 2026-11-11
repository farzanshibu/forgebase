/** Readable cells for openCypher results: vertex, edge and path values (FB-163) render as short text, anything else as JSON. */
import type { ReactNode } from "react";

type Json = Record<string, unknown>;
const isObj = (v: unknown): v is Json => typeof v === "object" && v !== null && !Array.isArray(v);
const str = (v: unknown) => (typeof v === "string" || typeof v === "number" ? String(v) : "?");

export interface Vertex { id: unknown; label: unknown; properties: Json }
export interface Edge { id: unknown; type: unknown; source: unknown; target: unknown; directed?: boolean; properties: Json }
export interface PathValue { nodes: Vertex[]; edges: Edge[] }

export const isVertex = (v: unknown): v is Vertex => isObj(v) && "id" in v && "label" in v && isObj(v.properties) && !("source" in v);
export const isEdge = (v: unknown): v is Edge => isObj(v) && "id" in v && "type" in v && "source" in v && "target" in v;
export const isPath = (v: unknown): v is PathValue => isObj(v) && Array.isArray(v.nodes) && Array.isArray(v.edges) && Object.keys(v).length === 2;

const hasProps = (p: Json | undefined) => !!p && Object.keys(p).length > 0;
const propsText = (p: Json | undefined) => (hasProps(p) ? JSON.stringify(p) : "");

function Props({ p }: { p: Json | undefined }) {
  return hasProps(p) ? <span className="ml-1 text-xs text-muted-foreground">{propsText(p)}</span> : null;
}

function VertexChip({ v }: { v: Vertex }) {
  return (
    <span data-kind="vertex" className="whitespace-nowrap">
      <span className="rounded bg-primary/10 px-1.5 py-0.5 font-mono text-xs">({str(v.id)}{v.label ? `:${str(v.label)}` : ""})</span>
      <Props p={v.properties} />
    </span>
  );
}

function arrow(e: Edge, from: string): string {
  const label = `[${str(e.type)}]`;
  if (e.directed === false) return `-${label}-`;
  return String(e.source) === from ? `-${label}->` : `<-${label}-`;
}

function EdgeChip({ e }: { e: Edge }) {
  return (
    <span data-kind="edge" className="whitespace-nowrap">
      <span className="font-mono text-xs">({str(e.source)}){arrow(e, String(e.source))}({str(e.target)})</span>
      <Props p={e.properties} />
    </span>
  );
}

function PathChain({ p }: { p: PathValue }) {
  const parts: ReactNode[] = [];
  p.nodes.forEach((n, i) => {
    parts.push(<span key={`n${i}`}>({str(n.id)}{n.label ? `:${str(n.label)}` : ""})</span>);
    const e = p.edges[i];
    if (e && i < p.nodes.length - 1) parts.push(<span key={`e${i}`} className="text-muted-foreground">{arrow(e, String(n.id))}</span>);
  });
  return (
    <span data-kind="path" className="whitespace-nowrap font-mono text-xs">
      {parts}
      <span className="ml-2 font-sans text-muted-foreground">{p.edges.length} {p.edges.length === 1 ? "hop" : "hops"}</span>
    </span>
  );
}

/** One result cell. Vertices read `(id:Label)`, edges `(a)-[TYPE]->(b)`, paths a chain of both; properties follow in grey. */
export function Cell({ value }: { value: unknown }): ReactNode {
  if (value === null || value === undefined) return <span className="text-muted-foreground">null</span>;
  if (isPath(value)) return <PathChain p={value} />;
  if (isVertex(value)) return <VertexChip v={value} />;
  if (isEdge(value)) return <EdgeChip e={value} />;
  if (typeof value === "string") return <span>{value}</span>;
  if (typeof value === "number" || typeof value === "boolean") return <span className="font-mono">{String(value)}</span>;
  if (Array.isArray(value)) return <span className="font-mono text-xs">[{value.map((x, i) => <span key={i}>{i > 0 ? ", " : ""}<Cell value={x} /></span>)}]</span>;
  return <span className="font-mono text-xs">{JSON.stringify(value)}</span>;
}
