import type { RequestOptions, Transport } from "./http";
import type { Schemas } from "./types";

const enc = encodeURIComponent;

/** A record as the API returns it: system fields plus the collection's own. */
export type Row = Record<string, unknown>;

export interface ListOptions {
  /** Filter-language expression, e.g. `price > {:min} && status = "active"`. */
  filter?: string;
  /** Typed values for `{:name}` placeholders in `filter` (sent with the JSON query endpoint). */
  params?: Record<string, unknown>;
  /** `"-created,name"` or `["-created", "name"]`. */
  sort?: string | string[];
  /** Relations to expand inline. */
  expand?: string[];
  /** Fields to return. */
  fields?: string[];
  /** Full-text search term. */
  search?: string;
  /** 1-based page (offset pagination). */
  page?: number;
  /** Page size (server maximum 500). */
  perPage?: number;
  /** Cursor from a previous page's `nextCursor`. */
  cursor?: string;
  /** Skip the total count (cheaper on large collections). */
  skipTotal?: boolean;
  signal?: AbortSignal;
}

export interface ListResult<T = Row> {
  items: T[];
  page?: number;
  perPage: number;
  totalItems?: number;
  totalPages?: number;
  /** Pass as `cursor` for the next page; absent on the last page. */
  nextCursor?: string;
}

export interface WriteOptions {
  /** Optimistic concurrency: send `If-Match` with the record's `_version`; a stale write throws `ConflictError`. */
  version?: number;
  signal?: AbortSignal;
}

export type TransferFormat = "json" | "ndjson" | "csv";

interface WireList<T> {
  items: T[];
  page?: number | null;
  per_page: number;
  total_items?: number | null;
  total_pages?: number | null;
  next_cursor?: string | null;
}

function fromWire<T>(w: WireList<T>): ListResult<T> {
  return {
    items: w.items,
    page: w.page ?? undefined,
    perPage: w.per_page,
    totalItems: w.total_items ?? undefined,
    totalPages: w.total_pages ?? undefined,
    nextCursor: w.next_cursor ?? undefined,
  };
}

const sortString = (s: ListOptions["sort"]) => (Array.isArray(s) ? s.join(",") : s);

/** Records of one collection: `client.collection("posts")`. */
export class RecordsApi<T extends Row = Row> {
  private readonly base: string;

  constructor(
    private readonly t: Transport,
    readonly name: string,
  ) {
    this.base = `/api/v1/collections/${enc(name)}`;
  }

  /** One page. With `params` the typed JSON query endpoint is used, otherwise a plain GET. */
  async list(o: ListOptions = {}): Promise<ListResult<T>> {
    const common = {
      filter: o.filter,
      sort: sortString(o.sort),
      search: o.search,
      page: o.page,
      per_page: o.perPage,
      cursor: o.cursor,
      skip_total: o.skipTotal,
    };
    if (o.params) {
      return fromWire(
        await this.t.request<WireList<T>>(`${this.base}/records/query`, {
          method: "POST",
          signal: o.signal,
          body: { ...common, params: o.params, expand: o.expand, fields: o.fields },
        }),
      );
    }
    return fromWire(
      await this.t.request<WireList<T>>(`${this.base}/records`, {
        signal: o.signal,
        query: { ...common, expand: o.expand?.join(","), fields: o.fields?.join(",") },
      }),
    );
  }

  /** Every page, lazily: `for await (const page of posts.pages({ perPage: 100 }))`. Follows `nextCursor`, else page numbers. */
  async *pages(o: ListOptions = {}): AsyncGenerator<ListResult<T>, void, undefined> {
    let cursor = o.cursor;
    let page = o.page ?? 1;
    for (;;) {
      const res = await this.list({ ...o, cursor, page: cursor ? undefined : page });
      yield res;
      if (res.nextCursor) cursor = res.nextCursor;
      else if (!cursor && res.page !== undefined && res.totalPages !== undefined && res.page < res.totalPages) page = res.page + 1;
      else return;
    }
  }

  /** Every record, lazily: `for await (const post of posts.iterate({ filter: "published = true" }))`. */
  async *iterate(o: ListOptions = {}): AsyncGenerator<T, void, undefined> {
    for await (const p of this.pages(o)) yield* p.items;
  }

  /** Collects every record (mind the memory use on big collections). */
  async all(o: ListOptions = {}): Promise<T[]> {
    const out: T[] = [];
    for await (const r of this.iterate(o)) out.push(r);
    return out;
  }

  /** First record matching `filter`, or `undefined`. */
  async first(filter: string, o: Omit<ListOptions, "filter" | "perPage" | "page" | "cursor"> = {}): Promise<T | undefined> {
    return (await this.list({ ...o, filter, perPage: 1, skipTotal: true })).items[0];
  }

  get(id: string, o: { expand?: string[]; fields?: string[]; signal?: AbortSignal } = {}): Promise<T> {
    return this.t.request<T>(`${this.base}/records/${enc(id)}`, {
      signal: o.signal,
      query: { expand: o.expand?.join(","), fields: o.fields?.join(",") },
    });
  }

  create(data: Partial<T> | Row, o: { signal?: AbortSignal } = {}): Promise<T> {
    return this.t.request<T>(`${this.base}/records`, { method: "POST", body: data, signal: o.signal });
  }

  /** Partial update (PATCH). */
  update(id: string, patch: Partial<T> | Row, o: WriteOptions = {}): Promise<T> {
    return this.t.request<T>(`${this.base}/records/${enc(id)}`, { method: "PATCH", body: patch, ...this.write(o) });
  }

  /** Full replace (PUT). */
  replace(id: string, data: Partial<T> | Row, o: WriteOptions = {}): Promise<T> {
    return this.t.request<T>(`${this.base}/records/${enc(id)}`, { method: "PUT", body: data, ...this.write(o) });
  }

  delete(id: string, o: { signal?: AbortSignal } = {}): Promise<void> {
    return this.t.request<void>(`${this.base}/records/${enc(id)}`, { method: "DELETE", signal: o.signal });
  }

  async count(filter?: string): Promise<number> {
    return (await this.t.request<{ count: number }>(`${this.base}/records/count`, { query: { filter } })).count;
  }

  /** Download the collection as text. */
  export(format: TransferFormat = "json", filter?: string): Promise<string> {
    return this.t.request<string>(`${this.base}/export`, { query: { format, filter }, text: true });
  }

  /** Upload records as text in the given format. */
  import(format: TransferFormat, body: string): Promise<Schemas["ImportResult"]> {
    return this.t.request(`${this.base}/import`, {
      method: "POST",
      query: { format },
      rawBody: body,
      headers: { "Content-Type": "application/octet-stream" },
    });
  }

  private write(o: WriteOptions): Pick<RequestOptions, "headers" | "signal"> {
    return { signal: o.signal, headers: o.version === undefined ? undefined : { "If-Match": `"${o.version}"` } };
  }
}

/** Collection definitions (`client.collections`). */
export class CollectionsApi {
  constructor(private readonly t: Transport) {}

  async list(): Promise<Schemas["CollectionSchema"][]> {
    return (await this.t.request<{ items: Schemas["CollectionSchema"][] }>("/api/v1/collections")).items;
  }

  get(name: string): Promise<Schemas["CollectionSchema"]> {
    return this.t.request(`/api/v1/collections/${enc(name)}`);
  }

  create(def: Schemas["NewCollection"]): Promise<Schemas["CollectionSchema"]> {
    return this.t.request("/api/v1/collections", { method: "POST", body: def });
  }

  update(name: string, def: Schemas["NewCollection"]): Promise<Schemas["CollectionSchema"]> {
    return this.t.request(`/api/v1/collections/${enc(name)}`, { method: "PUT", body: def });
  }

  delete(name: string): Promise<void> {
    return this.t.request(`/api/v1/collections/${enc(name)}`, { method: "DELETE" });
  }
}
