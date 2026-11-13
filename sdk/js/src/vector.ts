import type { Transport } from "./http";
import type { Schemas } from "./types";

/*
 * Vector search (`/api/v1/vector`): collections with one or more vector fields, upsert, k-nearest-neighbour
 * search with metadata filters, keyword + vector hybrid search, index maintenance: `client.vector`.
 *
 * Generated schemas type metadata as `Record<string, never>`; the aliases below widen it.
 */

/** Distance function of a vector field. `cosine`: `score` is the cosine similarity; `l2`: `1 / (1 + distance)`; `dot`: the dot product. */
export type VectorMetric = Schemas["Metric"];
/** Element type of the ANN index (canonical vectors are always float32). */
export type VectorQuantization = Schemas["Quantization"];
/** When a search filter is applied: `auto` (default), `pre` (filter first, exact when few match) or `post` (search first, widen until `k` match). */
export type VectorFilterMode = Schemas["FilterMode"];
/** How a search was answered. */
export type VectorStrategy = Schemas["Strategy"];
/** How the words of a hybrid keyword query combine: `or` (default, more matching words rank higher) or `and`. */
export type TextOperator = Schemas["TextOperator"];

export type VectorField = Schemas["VectorField"];
export type VectorCollection = Schemas["VectorCollection"];
export type VectorUpsertResult = Schemas["UpsertResult"];
export type VectorIndexStatus = Schemas["IndexStatus"];
export type VectorCollectionStats = Schemas["CollectionStats"];
export type VectorStats = Schemas["VectorStats"];
export type VectorRecallReport = Schemas["RecallReport"];

/** A stored item with its canonical vectors per field. */
export type VectorItem = Omit<Schemas["Item"], "metadata"> & { metadata: Record<string, unknown> };
export type SearchHit = Omit<Schemas["SearchHit"], "metadata"> & { metadata?: Record<string, unknown> | null };
export type SearchResponse = Omit<Schemas["SearchResponse"], "hits"> & { hits: SearchHit[] };
export type HybridHit = Omit<Schemas["HybridHit"], "metadata"> & { metadata?: Record<string, unknown> | null };
export type HybridResponse = Omit<Schemas["HybridResponse"], "hits"> & { hits: HybridHit[] };

/** A vector field to create. */
export interface FieldSpec {
  /** `[A-Za-z][A-Za-z0-9_]{0,62}`. */
  name: string;
  /** Components, 1 to 4096. */
  dimension: number;
  metric: VectorMetric;
  /** Element type of the ANN index (default `f32`). */
  quantization?: VectorQuantization;
  /** HNSW links per node (default 16, 2 to 128). */
  connectivity?: number;
  /** HNSW candidate list size while inserting (default 128). */
  expansionAdd?: number;
  /** HNSW candidate list size while searching (default 128): higher is better recall, slower. */
  expansionSearch?: number;
}

export interface NewVectorCollection {
  /** `[A-Za-z][A-Za-z0-9_]{0,62}`, unique per tenant. */
  name: string;
  description?: string;
  /** Up to 16 vector fields (more can be added later with `fields.add`). */
  fields?: FieldSpec[];
  /** Searches over at most this many candidates are exact (brute force) instead of approximate. */
  exactThreshold?: number;
  /**
   * Data collection the items are copied from (item id = record id). Searches then re-check every item
   * against that collection's View rule as the caller. Set at creation only.
   */
  sourceCollection?: string;
}

/** One item to insert or replace. */
export interface UpsertItem {
  /** 1 to 64 of `[A-Za-z0-9_-]`; generated when absent. An existing item with this id is replaced completely. */
  id?: string;
  /** Vector per field name; fields left out have no vector for this item. Components must be finite and match the field's dimension. */
  vectors?: Record<string, number[]>;
  /** Filterable JSON object (up to 64 KiB). */
  metadata?: Record<string, unknown>;
  /** Text for the keyword side of hybrid search. */
  text?: string;
}

interface QueryCommon {
  /** Vector field; may be left out when the collection has exactly one. */
  field?: string;
  /** Number of results, 1 to 1000 (default 10). */
  k?: number;
  /** Metadata filter, e.g. `category = 'news' && year >= {:min}`. */
  filter?: string;
  /** Values of the `{:name}` placeholders of `filter`. */
  params?: Record<string, unknown>;
  filterMode?: VectorFilterMode;
  /** Return each hit's vector. */
  includeVectors?: boolean;
  /** Return each hit's metadata (server default `true`). */
  includeMetadata?: boolean;
  /** Return each hit's text. */
  includeText?: boolean;
  signal?: AbortSignal;
}

export interface SearchOptions extends QueryCommon {
  /** Query vector. */
  vector: number[];
  /** Brute force instead of the ANN index (ground truth, small sets). */
  exact?: boolean;
}

export interface HybridOptions extends QueryCommon {
  /** Query vector; without it the search is keyword-only. */
  vector?: number[];
  /** Keyword query (every word matched literally); without it the search is vector-only. */
  text?: string;
  textOperator?: TextOperator;
  /** Candidates taken from each ranking before fusion (default `max(5k, 50)`, at most 1000). */
  candidates?: number;
  /** Reciprocal Rank Fusion constant (default 60): `score = Σ weight / (rrfK + rank)`. */
  rrfK?: number;
  /** Weights of the two rankings (default 1 and 1). */
  weights?: { vector?: number; text?: number };
}

export interface RecallOptions {
  /** Vector field; may be left out when the collection has exactly one. */
  field?: string;
  /** Queries (stored vectors), default 100, at most 1000. */
  samples?: number;
  /** Results per query, default 10, at most 100. */
  k?: number;
}

const seg = encodeURIComponent;
const V = "/api/v1/vector";

const fieldWire = (spec: FieldSpec) => ({
  name: spec.name,
  dimension: spec.dimension,
  metric: spec.metric,
  quantization: spec.quantization,
  connectivity: spec.connectivity,
  expansion_add: spec.expansionAdd,
  expansion_search: spec.expansionSearch,
});

/** Vector fields of one collection: `client.vector.collection("docs").fields`. */
export class VectorFieldsApi {
  private readonly base: string;
  constructor(
    private readonly t: Transport,
    name: string,
  ) {
    this.base = `${V}/collections/${seg(name)}/fields`;
  }

  async list(): Promise<VectorField[]> {
    return (await this.t.request<{ items: VectorField[] }>(this.base)).items;
  }

  /** Adds a field. Existing items have no vector for it until upserted again. */
  add(spec: FieldSpec): Promise<VectorField> {
    return this.t.request(this.base, {
      method: "POST",
      body: fieldWire(spec),
    });
  }

  /** Drops a field, its vectors and its index. */
  drop(field: string): Promise<void> {
    return this.t.request(`${this.base}/${seg(field)}`, { method: "DELETE" });
  }
}

/** One vector collection: `client.vector.collection("docs")`. */
export class VectorCollectionApi {
  readonly fields: VectorFieldsApi;
  private readonly base: string;

  constructor(
    private readonly t: Transport,
    readonly name: string,
  ) {
    this.base = `${V}/collections/${seg(name)}`;
    this.fields = new VectorFieldsApi(t, name);
  }

  /** The collection with its fields and item count. */
  get(): Promise<VectorCollection> {
    return this.t.request(this.base);
  }

  /** Drops the collection with all its items, vectors and index files. */
  drop(): Promise<void> {
    return this.t.request(this.base, { method: "DELETE" });
  }

  /** Inserts or replaces 1 to 1000 items in one transaction. The result lists the ids in request order. */
  upsert(items: UpsertItem | UpsertItem[]): Promise<VectorUpsertResult> {
    const list = Array.isArray(items) ? items : [items];
    return this.t.request(`${this.base}/upsert`, { method: "POST", body: { items: list } });
  }

  /** One item with its canonical vectors. */
  getItem(id: string): Promise<VectorItem> {
    return this.t.request(`${this.base}/items/${seg(id)}`);
  }

  /** Deletes one item (a missing id is a `NotFoundError`). */
  deleteItem(id: string): Promise<void> {
    return this.t.request(`${this.base}/items/${seg(id)}`, { method: "DELETE" });
  }

  /** Deletes 1 to 1000 items by id (`POST …/delete`); unknown ids are ignored. */
  deleteItems(ids: string[]): Promise<{ deleted: number }> {
    return this.t.request(`${this.base}/delete`, { method: "POST", body: { ids } });
  }

  /** k-nearest-neighbour search with an optional metadata filter; closest first. */
  search(o: SearchOptions): Promise<SearchResponse> {
    return this.t.request(`${this.base}/search`, {
      method: "POST",
      signal: o.signal,
      body: {
        field: o.field,
        vector: o.vector,
        k: o.k,
        filter: o.filter,
        params: o.params,
        filter_mode: o.filterMode,
        exact: o.exact,
        include_vectors: o.includeVectors,
        include_metadata: o.includeMetadata,
        include_text: o.includeText,
      },
    });
  }

  /** Hybrid search: keyword (BM25) and vector rankings fused with Reciprocal Rank Fusion; best first. */
  hybrid(o: HybridOptions): Promise<HybridResponse> {
    return this.t.request(`${this.base}/hybrid`, {
      method: "POST",
      signal: o.signal,
      body: {
        field: o.field,
        vector: o.vector,
        text: o.text,
        text_operator: o.textOperator,
        k: o.k,
        candidates: o.candidates,
        rrf_k: o.rrfK,
        weights: o.weights,
        filter: o.filter,
        params: o.params,
        filter_mode: o.filterMode,
        include_vectors: o.includeVectors,
        include_metadata: o.includeMetadata,
        include_text: o.includeText,
      },
    });
  }

  /** Item counts and the state, progress and metrics of each field's index. */
  stats(): Promise<VectorCollectionStats> {
    return this.t.request(`${this.base}/stats`);
  }

  /** Rebuilds ANN indexes from the canonical vectors in the background (202); follow `progress` in `stats()`. */
  async rebuild(field?: string): Promise<VectorIndexStatus[]> {
    return (await this.t.request<{ indexes: VectorIndexStatus[] }>(`${this.base}/rebuild`, { method: "POST", body: { field } })).indexes;
  }

  /** Measures recall@k of the ANN index against exact search, using stored vectors as queries. */
  recall(o: RecallOptions = {}): Promise<VectorRecallReport> {
    return this.t.request(`${this.base}/recall`, { method: "POST", body: { field: o.field, samples: o.samples, k: o.k } });
  }
}

/** Vector search: `client.vector`. */
export class VectorApi {
  constructor(private readonly t: Transport) {}

  /** A collection handle: `fb.vector.collection("docs").search({ vector })`. */
  collection(name: string): VectorCollectionApi {
    return new VectorCollectionApi(this.t, name);
  }

  /** Collections of the tenant, by name. */
  async list(): Promise<VectorCollection[]> {
    return (await this.t.request<{ items: VectorCollection[] }>(`${V}/collections`)).items;
  }

  /** Creates a collection with its vector fields. A taken name is a `ConflictError`. */
  create(input: NewVectorCollection): Promise<VectorCollection> {
    return this.t.request(`${V}/collections`, {
      method: "POST",
      body: {
        name: input.name,
        description: input.description,
        fields: input.fields?.map(fieldWire),
        exact_threshold: input.exactThreshold,
        source_collection: input.sourceCollection,
      },
    });
  }

  /** Totals over the tenant: collections, indexes, indexes building, memory. */
  stats(): Promise<VectorStats> {
    return this.t.request(`${V}/stats`);
  }
}
