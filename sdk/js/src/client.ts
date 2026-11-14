import { AccessApi } from "./access";
import { AlertsApi, SlosApi } from "./alerts";
import { InsightsApi } from "./insights";
import { CreditsApi } from "./credits";
import { CustomizeApi } from "./customize";
import { DocumentsApi } from "./documents";
import { FunctionsApi } from "./functions";
import { GraphApi } from "./graph";
import { IdentityApi } from "./identity";
import { IndexesApi } from "./indexes";
import { KvApi } from "./kv";
import { OrgsApi } from "./orgs";
import { SqlApi } from "./sql";
import { StorageApi } from "./storage";
import { SystemApi } from "./system";
import { VectorApi } from "./vector";
import { AuthApi, AuthManager, type AuthOptions } from "./auth";
import { BillingApi } from "./billing";
import type { CommitTokens, Consistency } from "./consistency";
import { EntitlementsApi } from "./entitlements";
import { Transport, type FetchLike, type RetryOptions } from "./http";
import { CollectionsApi, RecordsApi, type Row } from "./records";
import { JobsApi } from "./jobs";
import { QueuesApi } from "./queues";
import { RealtimeClient, type WebSocketCtor, type RealtimeTransport } from "./realtime";
import type { components } from "./generated/openapi";

export interface ForgeBaseOptions extends AuthOptions {
  /** Retry policy for 429 / 503 (and idempotent 502 / 504). */
  retry?: RetryOptions;
  /** Custom `fetch` (tests, proxies, older runtimes). */
  fetch?: FetchLike;
  /** Headers sent with every request (`X-Forgebase-Org`, tenant selection, ...). */
  headers?: Record<string, string>;
  /**
   * Read replicas (ADR-0029 §2.6): `"session"` (default) reads your own writes, through the commit
   * tokens the SDK tracks; `"strong"` reads every call from the primary. Per call: `{ consistency }`.
   */
  consistency?: Consistency;
  realtime?: {
    transport?: RealtimeTransport;
    /** WebSocket constructor for runtimes without a global one (`import WebSocket from "ws"`). */
    WebSocket?: WebSocketCtor;
    fallbackAfter?: number;
    reconnect?: { baseDelayMs?: number; maxDelayMs?: number; maxAttempts?: number };
  };
}

/** Entry point: `const fb = new ForgeBase("http://localhost:8080", { apiKey })`. */
export class ForgeBase {
  readonly auth: AuthApi;
  readonly collections: CollectionsApi;
  readonly entitlements: EntitlementsApi;
  readonly billing: BillingApi;
  readonly realtime: RealtimeClient;
  /** Durable topics and queues (`/api/v1/queues`). */
  readonly queues: QueuesApi;
  /** Background jobs, long tasks and workflows (`/api/v1/jobs`). */
  readonly jobs: JobsApi;
  /** Buckets, objects, multipart and resumable uploads, signed URLs (`/api/v1/storage`). */
  readonly storage: StorageApi;
  /** Key-value engine: `fb.kv.ns("cache")` (`/api/v1/kv`). */
  readonly kv: KvApi;
  /** Server functions: deploy, invoke, versions, secrets (`/api/v1/functions`). */
  readonly functions: FunctionsApi;
  /** Vector collections and similarity search (`/api/v1/vector`). */
  readonly vector: VectorApi;
  /** Property graph and openCypher (`/api/v1/graph`). */
  readonly graph: GraphApi;
  /** SQL editor (`/api/v1/sql`). */
  readonly sql: SqlApi;
  /** Collection indexes and the slow-query log (`/api/v1/indexes`). */
  readonly indexes: IndexesApi;
  /** Document-style access to collections (`/api/v1/documents`). */
  readonly documents: DocumentsApi;
  /** Prepaid credit balances and ledger (`/api/v1/credits`). */
  readonly credits: CreditsApi;
  /** Organizations, projects, teams, members, roles (`/api/v1/orgs`, ...). */
  readonly orgs: OrgsApi;
  /** Permission checks, ACLs, collection and resource rules. */
  readonly access: AccessApi;
  /** API keys, tokens, service accounts, OAuth clients, realms, user administration (`/api/auth`). */
  readonly identity: IdentityApi;
  /** Domains, branding, email and agreements (`/api/v1/customize`). */
  readonly customize: CustomizeApi;
  /** Server administration: features, config, topology, backups, DB logins, extensions. */
  readonly system: SystemApi;
  /** Alert rules, alerts, silences and notifiers (`/api/v1/alerts`). Platform administrators. */
  readonly alerts: AlertsApi;
  /** Service level objectives and their status (`/api/v1/slos`). Platform administrators. */
  readonly slos: SlosApi;
  /** Statement statistics, regressions, usage, function profiles, audit search and export (`/api/v1/insights`). Administrators. */
  readonly insights: InsightsApi;
  readonly transport: Transport;
  private readonly mgr: AuthManager;

  constructor(
    readonly baseUrl: string,
    options: ForgeBaseOptions = {},
  ) {
    this.transport = new Transport({
      baseUrl,
      fetch: options.fetch,
      retry: options.retry,
      headers: options.headers,
      consistency: options.consistency,
    });
    this.mgr = new AuthManager(this.transport, options);
    this.transport.credentials = this.mgr;
    this.auth = new AuthApi(this.transport, this.mgr);
    this.collections = new CollectionsApi(this.transport);
    this.entitlements = new EntitlementsApi(this.transport);
    this.billing = new BillingApi(this.transport);
    this.storage = new StorageApi(this.transport);
    this.kv = new KvApi(this.transport, options.retry?.sleep);
    this.functions = new FunctionsApi(this.transport);
    this.vector = new VectorApi(this.transport);
    this.graph = new GraphApi(this.transport);
    this.sql = new SqlApi(this.transport);
    this.indexes = new IndexesApi(this.transport);
    this.documents = new DocumentsApi(this.transport);
    this.credits = new CreditsApi(this.transport);
    this.orgs = new OrgsApi(this.transport);
    this.access = new AccessApi(this.transport);
    this.identity = new IdentityApi(this.transport, this.mgr);
    this.customize = new CustomizeApi(this.transport);
    this.system = new SystemApi(this.transport);
    this.alerts = new AlertsApi(this.transport);
    this.slos = new SlosApi(this.transport);
    this.insights = new InsightsApi(this.transport);
    const mgr = this.mgr;
    this.realtime = new RealtimeClient({
      baseUrl: this.transport.baseUrl,
      fetch: options.fetch,
      commitTokens: this.transport.commitTokens,
      ...options.realtime,
      sleep: options.retry?.sleep,
      random: options.retry?.random,
      tokens: {
        async get() {
          const headers = await mgr.headers("GET");
          const token = mgr.apiKey ?? (await mgr.getAccessToken());
          return { token, headers };
        },
        async refresh() {
          if (!mgr.apiKey) await mgr.refresh();
        },
        onChange(fn) {
          return mgr.onChange((_e, s) => fn(s?.accessToken));
        },
      },
    });
    this.queues = new QueuesApi(this.transport, options.retry?.sleep);
    this.jobs = new JobsApi(this.transport, this.realtime, options.retry?.sleep);
  }

  /**
   * Commit tokens of this client's writes, sent as `X-ForgeBase-Read-After`. Realtime events that
   * carry a position can feed it: `fb.commitTokens.note(token)`.
   */
  get commitTokens(): CommitTokens {
    return this.transport.commitTokens;
  }

  /** Records of a collection: `fb.collection<Post>("posts").list({ sort: "-created" })`. */
  collection<T extends Row = Row>(name: string): RecordsApi<T> {
    return new RecordsApi<T>(this.transport, name);
  }

  /** Typed escape hatch for any route: `fb.request("/api/v1/kv/_info")`. */
  request<T = unknown>(path: string, opts?: Parameters<Transport["request"]>[1]): Promise<T> {
    return this.transport.request<T>(path, opts);
  }

  /** Server version and build info. */
  systemInfo(): Promise<components["schemas"]["Item"] | Record<string, unknown>> {
    return this.transport.request("/api/system/info");
  }

  /** Liveness probe: resolves when the server answers. */
  async health(): Promise<boolean> {
    try {
      await this.transport.request("/health", { anonymous: true, retry: false });
      return true;
    } catch {
      return false;
    }
  }
}

export const createClient = (baseUrl: string, options?: ForgeBaseOptions): ForgeBase => new ForgeBase(baseUrl, options);
