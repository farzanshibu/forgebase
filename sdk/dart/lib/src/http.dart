import 'dart:async';
import 'dart:convert';
import 'dart:math' as math;

import 'package:http/http.dart' as http;

import 'consistency.dart';
import 'errors.dart';

/// A decoded JSON object.
typedef Json = Map<String, dynamic>;

/// Retry policy for 429 / 503 (and idempotent 502 / 504).
class RetryOptions {
  /// Creates a retry policy.
  const RetryOptions({
    this.maxRetries = 3,
    this.baseDelay = const Duration(milliseconds: 250),
    this.maxDelay = const Duration(seconds: 10),
    this.maxRetryAfter = const Duration(seconds: 60),
    this.statuses = const {429, 503},
  });

  /// Retries after the first attempt (default 3). `0` disables retrying.
  final int maxRetries;

  /// First backoff delay; doubles each attempt with full jitter.
  final Duration baseDelay;

  /// Upper bound for one backoff delay.
  final Duration maxDelay;

  /// Longest `Retry-After` the SDK sleeps for; longer asks fail at once.
  final Duration maxRetryAfter;

  /// Statuses retried for every method. 502 and 504 are retried for
  /// idempotent methods only.
  final Set<int> statuses;

  /// No retries.
  static const none = RetryOptions(maxRetries: 0);
}

/// Sleeps for a duration; replace it in tests.
typedef Sleeper = Future<void> Function(Duration d);

/// Sleeps with a real timer.
Future<void> defaultSleep(Duration d) => Future<void>.delayed(d);

const _idempotent = {'GET', 'HEAD', 'OPTIONS', 'PUT', 'DELETE'};

/// What the transport needs from the auth layer.
abstract interface class Credentials {
  /// Headers for one request (may refresh the access token first).
  Future<Map<String, String>> headers(String method);

  /// Called after a 401 with the headers that were sent: returns `true` when a
  /// retry can succeed.
  Future<bool> onUnauthorized(Map<String, String> sent);
}

/// Builds a query string from non-null values.
String buildQuery(Map<String, Object?>? query) {
  if (query == null) return '';
  final parts = <String>[];
  for (final e in query.entries) {
    final v = e.value;
    if (v == null) continue;
    parts.add(
      '${Uri.encodeQueryComponent(e.key)}=${Uri.encodeQueryComponent('$v')}',
    );
  }
  return parts.join('&');
}

/// The HTTP layer: retries, error mapping, credentials, commit tokens.
class Transport {
  /// Creates a transport.
  Transport({
    required String baseUrl,
    http.Client? client,
    this.retry = const RetryOptions(),
    Sleeper? sleep,
    double Function()? random,
    Map<String, String>? headers,
    this.consistency = Consistency.session,
    CommitTokens? commitTokens,
    this.timeout = const Duration(seconds: 30),
  })  : baseUrl = baseUrl.replaceAll(RegExp(r'/+$'), ''),
        client = client ?? http.Client(),
        ownsClient = client == null,
        sleep = sleep ?? defaultSleep,
        random = random ?? math.Random().nextDouble,
        defaultHeaders = headers ?? const {},
        commitTokens = commitTokens ?? CommitTokens();

  /// Server root without trailing slash.
  final String baseUrl;

  /// The underlying client.
  final http.Client client;

  /// Whether [close] closes [client].
  final bool ownsClient;

  /// Default retry policy.
  final RetryOptions retry;

  /// Sleep used between retries.
  final Sleeper sleep;

  /// Random source for backoff jitter, `[0, 1)`.
  final double Function() random;

  /// Headers sent with every request.
  final Map<String, String> defaultHeaders;

  /// Commit tokens seen in responses.
  final CommitTokens commitTokens;

  /// Default consistency of every call.
  Consistency consistency;

  /// Time to wait for response headers (not for the body).
  final Duration? timeout;

  /// Set by the client once the auth manager exists.
  Credentials? credentials;

  /// Closes the HTTP client when the SDK created it.
  void close() {
    if (ownsClient) client.close();
  }

  /// `baseUrl + path (+ query)`.
  String url(String path, [Map<String, Object?>? query]) {
    final qs = buildQuery(query);
    final u = '$baseUrl$path';
    return qs.isEmpty ? u : '$u?$qs';
  }

  /// Delay before retry number [attempt] (1-based): `Retry-After` when sent,
  /// else exponential backoff with full jitter.
  Duration backoff(int attempt, Duration? retryAfter, [RetryOptions? cfg]) {
    if (retryAfter != null) return retryAfter;
    final c = cfg ?? retry;
    final ceiling = math.min(
      c.maxDelay.inMilliseconds,
      c.baseDelay.inMilliseconds * math.pow(2, attempt - 1).toInt(),
    );
    return Duration(milliseconds: (random() * ceiling).floor());
  }

  /// Sends a request and decodes the answer.
  ///
  /// Returns the parsed JSON (`null` for 204 / empty bodies), or the text when
  /// [text] is set.
  Future<Object?> request(
    String path, {
    String method = 'GET',
    Map<String, Object?>? query,
    Object? body,
    List<int>? rawBody,
    Map<String, String>? headers,
    bool text = false,
    bool anonymous = false,
    RetryOptions? retry,
    Consistency? consistency,
  }) async {
    final res = await send(
      path,
      method: method,
      query: query,
      body: body,
      rawBody: rawBody,
      headers: headers,
      anonymous: anonymous,
      retry: retry,
      consistency: consistency,
    );
    final bytes = await res.stream.toBytes();
    if (res.statusCode == 204 || bytes.isEmpty) return text ? '' : null;
    final s = utf8.decode(bytes, allowMalformed: true);
    if (text) return s;
    try {
      return jsonDecode(s);
    } on FormatException {
      throw InvalidResponseException(
        'response was not valid JSON',
        status: res.statusCode,
      );
    }
  }

  /// [request] expecting a JSON object.
  Future<Json> requestJson(
    String path, {
    String method = 'GET',
    Map<String, Object?>? query,
    Object? body,
    List<int>? rawBody,
    Map<String, String>? headers,
    bool anonymous = false,
    RetryOptions? retry,
    Consistency? consistency,
  }) async {
    final r = await request(
      path,
      method: method,
      query: query,
      body: body,
      rawBody: rawBody,
      headers: headers,
      anonymous: anonymous,
      retry: retry,
      consistency: consistency,
    );
    if (r is Json) return r;
    throw InvalidResponseException('expected a JSON object from $path');
  }

  /// Sends with retries; returns a 2xx streamed response, throws a typed
  /// exception otherwise. The caller must drain `stream`.
  Future<http.StreamedResponse> send(
    String path, {
    String method = 'GET',
    Map<String, Object?>? query,
    Object? body,
    List<int>? rawBody,
    Map<String, String>? headers,
    bool anonymous = false,
    RetryOptions? retry,
    Consistency? consistency,
    String? absoluteUrl,
    Duration? headersTimeout,
  }) async {
    final m = method.toUpperCase();
    final cfg = retry ?? this.retry;
    final target = Uri.parse(absoluteUrl ?? url(path, query));
    var refreshed = false;
    var attempt = 0;
    while (true) {
      final h = <String, String>{
        'Accept': 'application/json',
        ...defaultHeaders,
      };
      List<int>? payload;
      if (rawBody != null) {
        payload = rawBody;
      } else if (body != null) {
        h['Content-Type'] = 'application/json';
        payload = utf8.encode(jsonEncode(body));
      }
      if (!anonymous && credentials != null) {
        h.addAll(await credentials!.headers(m));
      }
      final readAfter = commitTokens.header();
      if (readAfter != null) h[readAfterHeader] = readAfter;
      if ((consistency ?? this.consistency) == Consistency.strong) {
        h[consistencyHeader] = 'strong';
      }
      if (headers != null) h.addAll(headers);

      final req = http.Request(m, target)..headers.addAll(h);
      if (payload != null) req.bodyBytes = payload;

      http.StreamedResponse res;
      try {
        final f = client.send(req);
        final t = headersTimeout ?? timeout;
        res = await (t == null ? f : f.timeout(t));
      } on Exception catch (err) {
        if (attempt < cfg.maxRetries && _idempotent.contains(m)) {
          attempt++;
          await sleep(backoff(attempt, null, cfg));
          continue;
        }
        throw NetworkException(
          err is TimeoutException
              ? 'request timed out'
              : (err.toString().isEmpty ? 'network request failed' : '$err'),
          cause: err,
        );
      }
      // A failed request may still have committed (a batch, a partial import).
      commitTokens.note(res.headers[commitHeader.toLowerCase()]);
      if (res.statusCode >= 200 && res.statusCode < 300) return res;

      if (res.statusCode == 401 &&
          !anonymous &&
          !refreshed &&
          credentials != null &&
          await credentials!.onUnauthorized(h)) {
        refreshed = true;
        await _drain(res);
        continue;
      }

      final retriable = cfg.statuses.contains(res.statusCode) ||
          ((res.statusCode == 502 || res.statusCode == 504) &&
              _idempotent.contains(m));
      // The body is read once: a quota 429 must not be retried.
      final errBytes = await _readAll(res);
      final errJson =
          tryDecodeJson(utf8.decode(errBytes, allowMalformed: true));
      final err = errorFromResponse(res.statusCode, errJson, res.headers);
      if (retriable &&
          attempt < cfg.maxRetries &&
          err is! QuotaExceededException) {
        final ra = parseRetryAfter(res.headers['retry-after']);
        if (ra == null || ra <= cfg.maxRetryAfter) {
          attempt++;
          await sleep(backoff(attempt, ra, cfg));
          continue;
        }
      }
      throw err;
    }
  }

  Future<List<int>> _readAll(http.StreamedResponse res) async {
    try {
      return await res.stream.toBytes();
    } on Object {
      // An unreadable error body still maps to a typed error.
      return const [];
    }
  }

  Future<void> _drain(http.StreamedResponse res) async {
    try {
      await res.stream.drain<void>();
    } on Object {
      // The body is not needed.
    }
  }
}
