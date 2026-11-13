import 'dart:convert';
import 'dart:typed_data';

import 'http.dart';

String _enc(String s) => Uri.encodeComponent(s);

/// The raw answer of a function.
class FunctionResponse {
  /// Creates a response.
  const FunctionResponse(this.status, this.headers, this.bytes);

  /// HTTP status.
  final int status;

  /// Response headers (lower-case names).
  final Map<String, String> headers;

  /// Body bytes.
  final Uint8List bytes;

  /// The body as text.
  String get text => utf8.decode(bytes, allowMalformed: true);

  /// The body as JSON.
  Object? get json => bytes.isEmpty ? null : jsonDecode(text);
}

/// Server functions: `client.functions`.
class FunctionsApi {
  /// Creates the API.
  FunctionsApi(this._t);

  final Transport _t;

  /// Every function.
  Future<List<Json>> list() async {
    final r = await _t.requestJson('/api/v1/functions');
    return [
      for (final i in (r['items'] as List<dynamic>? ?? const [])) i as Json
    ];
  }

  /// One function.
  Future<Json> get(String name) =>
      _t.requestJson('/api/v1/functions/${_enc(name)}');

  /// Deploys a new version. [request] is the server's deploy body (`name` and
  /// `source` for JavaScript / TypeScript, or `name` and `wasm`).
  Future<Json> deploy(Json request) =>
      _t.requestJson('/api/v1/functions', method: 'POST', body: request);

  /// Deletes a function, its versions, secrets and logs.
  Future<void> delete(String name) async {
    await _t.request('/api/v1/functions/${_enc(name)}', method: 'DELETE');
  }

  /// Invokes a function with a JSON [payload] and returns its decoded JSON
  /// result (`null` for an empty body). [path] is a sub-path
  /// (`/invoke/<path>`); an administrator may pass a [version] to test.
  Future<Object?> invoke(
    String name, {
    Object? payload,
    String? path,
    int? version,
    Map<String, Object?>? query,
  }) =>
      _t.request(
        _invokePath(name, path),
        method: 'POST',
        body: payload,
        query: {...?query, 'version': version},
      );

  /// Like [invoke] but returns status, headers and bytes untouched, for
  /// functions that do not answer JSON.
  Future<FunctionResponse> invokeRaw(
    String name, {
    Object? payload,
    String? path,
    int? version,
    Map<String, Object?>? query,
    Map<String, String>? headers,
  }) async {
    final res = await _t.send(
      _invokePath(name, path),
      method: 'POST',
      body: payload,
      query: {...?query, 'version': version},
      headers: {'Accept': '*/*', ...?headers},
    );
    return FunctionResponse(
      res.statusCode,
      res.headers,
      await res.stream.toBytes(),
    );
  }

  String _invokePath(String name, String? path) {
    final base = '/api/v1/functions/${_enc(name)}/invoke';
    if (path == null || path.isEmpty || path == '/') return base;
    final p = path.startsWith('/') ? path.substring(1) : path;
    return '$base/${p.split('/').map(_enc).join('/')}';
  }

  /// Recent executions, newest first.
  Future<List<Json>> logs(String name, {int? limit}) async {
    final r = await _t.requestJson(
      '/api/v1/functions/${_enc(name)}/logs',
      query: {'limit': limit},
    );
    return [
      for (final i in (r['items'] as List<dynamic>? ?? const [])) i as Json
    ];
  }

  /// A function's versions, newest first.
  Future<List<Json>> versions(String name) async {
    final r = await _t.requestJson('/api/v1/functions/${_enc(name)}/versions');
    return [
      for (final i in (r['items'] as List<dynamic>? ?? const [])) i as Json
    ];
  }

  /// Activates an older version (the previous one when [version] is `null`).
  Future<Json> rollback(String name, {int? version}) => _t.requestJson(
        '/api/v1/functions/${_enc(name)}/rollback',
        method: 'POST',
        body: {if (version != null) 'version': version},
      );

  /// Secret names of a function (values are never returned).
  Future<List<Json>> secrets(String name) async {
    final r = await _t.requestJson('/api/v1/functions/${_enc(name)}/secrets');
    return [
      for (final i in (r['items'] as List<dynamic>? ?? const [])) i as Json
    ];
  }

  /// Sets a secret.
  Future<Json> putSecret(String name, String key, String value) =>
      _t.requestJson(
        '/api/v1/functions/${_enc(name)}/secrets/${_enc(key)}',
        method: 'PUT',
        body: {'value': value},
      );

  /// Deletes a secret.
  Future<void> deleteSecret(String name, String key) async {
    await _t.request(
      '/api/v1/functions/${_enc(name)}/secrets/${_enc(key)}',
      method: 'DELETE',
    );
  }
}
