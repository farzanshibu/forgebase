import 'dart:async';

import 'package:http/http.dart' as http;

import 'auth.dart';
import 'billing.dart';
import 'consistency.dart';
import 'entitlements.dart';
import 'functions.dart';
import 'http.dart';
import 'jobs.dart';
import 'kv.dart';
import 'queues.dart';
import 'realtime.dart';
import 'records.dart';
import 'storage.dart';
import 'token_storage.dart';

/// Entry point: `final fb = ForgeBase('http://localhost:8090', apiKey: ...)`.
class ForgeBase {
  /// Creates a client for the server at [baseUrl].
  ///
  /// Authenticate with [apiKey] (sent as `X-API-Key`), a static
  /// [accessToken], or sign in through [auth]; [storage] keeps the session
  /// between app launches (in memory by default).
  factory ForgeBase(
    String baseUrl, {
    String? apiKey,
    String? accessToken,
    TokenStorage? storage,
    Duration refreshSkew = const Duration(seconds: 30),
    bool autoRefresh = true,
    RetryOptions retry = const RetryOptions(),
    http.Client? httpClient,
    Map<String, String>? headers,
    Consistency consistency = Consistency.session,
    Duration? timeout = const Duration(seconds: 30),
    WebSocketConnector? webSocketConnector,
    ReconnectOptions reconnect = const ReconnectOptions(),
    Sleeper? sleep,
    double Function()? random,
    DateTime Function()? now,
  }) {
    final transport = Transport(
      baseUrl: baseUrl,
      client: httpClient,
      retry: retry,
      sleep: sleep,
      random: random,
      headers: headers,
      consistency: consistency,
      timeout: timeout,
    );
    final mgr = AuthManager(
      transport,
      AuthOptions(
        apiKey: apiKey,
        accessToken: accessToken,
        storage: storage,
        refreshSkew: refreshSkew,
        autoRefresh: autoRefresh,
        now: now,
      ),
    );
    transport.credentials = mgr;
    final realtime = RealtimeClient(
      transport: transport,
      tokens: _ManagerTokens(mgr),
      connector: webSocketConnector,
      reconnect: reconnect,
      sleep: sleep,
      random: random,
    );
    return ForgeBase._(transport, mgr, realtime, sleep);
  }

  ForgeBase._(this.transport, this._mgr, this.realtime, Sleeper? sleep)
      : auth = AuthApi(transport, _mgr),
        collections = CollectionsApi(transport),
        entitlements = EntitlementsApi(transport),
        billing = BillingApi(transport),
        queues = QueuesApi(transport, sleep: sleep),
        jobs = JobsApi(transport, realtime, sleep: sleep),
        storage = StorageApi(transport),
        functions = FunctionsApi(transport);

  /// The HTTP layer (retries, credentials, commit tokens).
  final Transport transport;
  final AuthManager _mgr;

  /// Sign-up, sign-in flows, sessions, tokens and account helpers.
  final AuthApi auth;

  /// Collection definitions.
  final CollectionsApi collections;

  /// Entitlement and quota helpers.
  final EntitlementsApi entitlements;

  /// Store purchases.
  final BillingApi billing;

  /// Realtime subscriptions, channels and presence.
  final RealtimeClient realtime;

  /// Durable topics and queues (`/api/v1/queues`).
  final QueuesApi queues;

  /// Background jobs, long tasks and workflows (`/api/v1/jobs`).
  final JobsApi jobs;

  /// Buckets, objects, signed URLs and uploads.
  final StorageApi storage;

  /// Server functions.
  final FunctionsApi functions;

  /// The server root without a trailing slash.
  String get baseUrl => transport.baseUrl;

  /// Commit tokens of this client's writes, sent as
  /// `X-ForgeBase-Read-After`.
  CommitTokens get commitTokens => transport.commitTokens;

  /// Default consistency of every call.
  Consistency get consistency => transport.consistency;
  set consistency(Consistency c) => transport.consistency = c;

  /// Records of a collection as maps: `fb.collection('posts').list(...)`.
  RecordsApi<Row> collection(String name) => RecordsApi<Row>(transport, name);

  /// Records of a collection mapped to your own type with [decode]:
  /// `fb.collectionOf('posts', decode: Post.fromJson)`.
  RecordsApi<T> collectionOf<T>(
    String name, {
    required T Function(Row row) decode,
  }) =>
      RecordsApi<T>(transport, name, decode: decode);

  /// One key-value namespace: `fb.kv('cache').set('k', 'v')`.
  KvApi kv(String namespace) => KvApi(transport, namespace);

  /// Typed escape hatch for any route: `fb.request('/api/v1/kv/_info')`.
  Future<Object?> request(
    String path, {
    String method = 'GET',
    Map<String, Object?>? query,
    Object? body,
    Map<String, String>? headers,
  }) =>
      transport.request(
        path,
        method: method,
        query: query,
        body: body,
        headers: headers,
      );

  /// Server version and build info.
  Future<Json> systemInfo() => transport.requestJson('/api/system/info');

  /// Liveness probe: completes with `true` when the server answers.
  Future<bool> health() async {
    try {
      await transport.request(
        '/health',
        anonymous: true,
        retry: RetryOptions.none,
        text: true,
      );
      return true;
    } on Object {
      return false;
    }
  }

  /// Closes the realtime connection and the HTTP client (when the SDK
  /// created it).
  Future<void> close() async {
    await realtime.dispose();
    await _mgr.dispose();
    transport.close();
  }
}

class _ManagerTokens implements TokenSource {
  _ManagerTokens(this._mgr);

  final AuthManager _mgr;

  @override
  Future<String?> get() async => _mgr.apiKey ?? await _mgr.getAccessToken();

  @override
  Future<void> refresh() async {
    if (_mgr.apiKey == null) await _mgr.refresh();
  }

  @override
  Stream<String?> get changes =>
      _mgr.changes.map((c) => c.session?.accessToken);
}
