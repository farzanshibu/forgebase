import type { Consistency } from "./consistency";
import type { Transport } from "./http";
import type { Row } from "./records";
import type { Schemas } from "./types";

/*
 * Documents (`/api/v1/documents/{collection}`): the record operations of a document collection, `client.documents`.
 *
 *   const notes = fb.documents.collection<Note>("notes");
 *   const n = await notes.create({ title: "Hi" });
 *   await notes.patch(n.id, { done: true }, { version: n._version });
 */

/** A stored version of a document (collections with `versioned: true`). */
export type DocumentVersion = Schemas["RecordVersionSchema"];

export interface DocumentWriteOptions {
  /** Optimistic concurrency: sent as `If-Match` with the document's `_version`; a stale write throws `ConflictError`. */
  version?: number;
  signal?: AbortSignal;
}

export interface GetDocumentOptions {
  /** Relations to expand inline. */
  expand?: string[];
  /** Fields to return. */
  fields?: string[];
  consistency?: Consistency;
  signal?: AbortSignal;
}

const enc = encodeURIComponent;
const ifMatch = (o: DocumentWriteOptions) => (o.version === undefined ? undefined : { "If-Match": `"${o.version}"` });

/** The documents of one collection: `client.documents.collection("notes")`. */
export class DocumentCollection<T extends Row = Row> {
  constructor(
    private readonly t: Transport,
    readonly name: string,
  ) {}

  private path(id?: string, suffix = ""): string {
    return `/api/v1/documents/${enc(this.name)}${id === undefined ? "" : `/${enc(id)}`}${suffix}`;
  }

  /** Creates one document. */
  create(doc: Partial<T> | Row, o: { signal?: AbortSignal } = {}): Promise<T> {
    return this.t.request<T>(this.path(), { method: "POST", body: doc, signal: o.signal });
  }

  /** Creates up to 1000 documents in one all-or-nothing transaction. */
  async createMany(docs: (Partial<T> | Row)[], o: { signal?: AbortSignal } = {}): Promise<T[]> {
    return (await this.t.request<{ items: T[] }>(this.path(), { method: "POST", body: docs, signal: o.signal })).items;
  }

  get(id: string, o: GetDocumentOptions = {}): Promise<T> {
    return this.t.request<T>(this.path(id), {
      signal: o.signal,
      consistency: o.consistency,
      query: { expand: o.expand?.join(","), fields: o.fields?.join(",") },
    });
  }

  /** Merge patch (RFC 7386): objects merge, `null` removes a field. */
  patch(id: string, patch: Partial<T> | Row, o: DocumentWriteOptions = {}): Promise<T> {
    return this.t.request<T>(this.path(id), { method: "PATCH", body: patch, headers: ifMatch(o), signal: o.signal });
  }

  /** Replaces the whole document. */
  replace(id: string, doc: Partial<T> | Row, o: DocumentWriteOptions = {}): Promise<T> {
    return this.t.request<T>(this.path(id), { method: "PUT", body: doc, headers: ifMatch(o), signal: o.signal });
  }

  delete(id: string, o: DocumentWriteOptions = {}): Promise<void> {
    return this.t.request<void>(this.path(id), { method: "DELETE", headers: ifMatch(o), signal: o.signal });
  }

  /** Stored versions of a document, oldest first (collections with `versioned: true`). */
  async versions(id: string): Promise<DocumentVersion[]> {
    return (await this.t.request<{ items: DocumentVersion[] }>(this.path(id, "/versions"))).items;
  }
}

/** Document collections: `client.documents`. */
export class DocumentsApi {
  constructor(private readonly t: Transport) {}

  /** One collection: `fb.documents.collection<Note>("notes").get(id)`. */
  collection<T extends Row = Row>(name: string): DocumentCollection<T> {
    return new DocumentCollection<T>(this.t, name);
  }
}
