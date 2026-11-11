/** Creating a vector collection and the sample data. Hand-written: `api/vector.ts` has no `source_collection` yet (FB-139). */
import { send } from "../../api/client";
import { createVectorCollection, upsertVectors } from "../../api";
import type { FieldSpec, Metric, Quantization, VectorCollection } from "../../api";

export interface CreateBody {
  name: string;
  description?: string;
  fields: FieldSpec[];
  /** A database collection whose View rule and field mask every hit is re-checked against (FB-139). */
  source_collection?: string;
}

/** The request body for the form's values: what "Create collection" sends, shown to the user before they send it. */
export function buildBody(v: { name: string; description: string; field: string; dimension: string; metric: Metric; quantization: Quantization; source: string }): CreateBody {
  return {
    name: v.name.trim(),
    ...(v.description.trim() ? { description: v.description.trim() } : {}),
    fields: v.field.trim() ? [{ name: v.field.trim(), dimension: Number(v.dimension), metric: v.metric, quantization: v.quantization }] : [],
    ...(v.source ? { source_collection: v.source } : {}),
  };
}

export const createCollectionBody = (body: CreateBody) => send<VectorCollection>("POST", "/api/v1/vector/collections", body);

export const SAMPLE_NAME = "sample_notes";
/** A three-dimension sample: six short notes, two themes, cosine distance. */
export async function loadSample(): Promise<void> {
  await createVectorCollection({ name: SAMPLE_NAME, description: "Sample notes with 3-dimension embeddings", fields: [{ name: "embedding", dimension: 3, metric: "cosine" }] });
  await upsertVectors(SAMPLE_NAME, [
    { id: "n1", vectors: { embedding: [0.9, 0.1, 0.0] }, metadata: { topic: "cooking" }, text: "Whisk the eggs before adding the butter" },
    { id: "n2", vectors: { embedding: [0.8, 0.2, 0.1] }, metadata: { topic: "cooking" }, text: "Roast vegetables at a high heat" },
    { id: "n3", vectors: { embedding: [0.85, 0.05, 0.1] }, metadata: { topic: "cooking" }, text: "Season the soup at the end" },
    { id: "n4", vectors: { embedding: [0.0, 0.9, 0.2] }, metadata: { topic: "travel" }, text: "The night train leaves at nine" },
    { id: "n5", vectors: { embedding: [0.1, 0.8, 0.3] }, metadata: { topic: "travel" }, text: "Book the window seat early" },
    { id: "n6", vectors: { embedding: [0.05, 0.85, 0.25] }, metadata: { topic: "travel" }, text: "Pack light for a week by the sea" },
  ]);
}
