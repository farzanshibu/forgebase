import 'dart:async';
import 'dart:convert';
import 'dart:typed_data';

import 'errors.dart';
import 'http.dart';
import 'sse.dart';

String _enc(String s) => Uri.encodeComponent(s);

/// Encodes a KV value for the wire: a `String` is sent as text, bytes
/// (`List<int>`) as `{"base64": "..."}`.
Object kvEncode(Object value) {
  if (value is String) return value;
  if (value is List<int>) return {'base64': base64Encode(value)};
  throw ArgumentError.value(value, 'value', 'expected a String or bytes');
}

/// Decodes a value the server returned: a `String`, or `Uint8List` for
/// `{"base64": "..."}`. Anything else is returned as is.
Object? kvDecode(Object? json) {
  if (json is Map<String, dynamic> && json['base64'] is String) {
    return base64Decode(json['base64'] as String);
  }
  return json;
}

Object _arg(Object a) => a is String || a is List<int> ? a : '$a';

/// A key of any type with its value, as `GET .../keys/{key}` returns it.
class KvKey {
  /// Creates a key.
  const KvKey({
    required this.key,
    required this.type,
    required this.size,
    required this.truncated,
    required this.value,
    this.ttl,
  });

  /// Reads the server's JSON.
  factory KvKey.fromJson(Json j) => KvKey(
        key: j['key'] as String? ?? '',
        type: j['type'] as String? ?? 'string',
        size: (j['size'] as num?)?.toInt() ?? 0,
        truncated: j['truncated'] == true,
        value: j['value'],
        ttl: j['ttl_ms'] is num
            ? Duration(milliseconds: (j['ttl_ms'] as num).toInt())
            : null,
      );

  /// The key.
  final String key;

  /// `string`, `hash`, `list`, `set`, `zset` or `stream`.
  final String type;

  /// Length: bytes, fields, items, members or entries.
  final int size;

  /// Whether [value] was cut to the read limit.
  final bool truncated;

  /// The value in the server's JSON shape (a string for strings,
  /// `{field: value}` for hashes, a list for lists, ...). Use [kvDecode] on
  /// binary values.
  final Object? value;

  /// Time until expiry; `null` when the key does not expire.
  final Duration? ttl;
}

/// A message published to a channel of a namespace.
class KvMessage {
  /// Creates a message.
  const KvMessage({required this.channel, required this.message});

  /// Channel name.
  final String channel;

  /// The message (`String` or `Uint8List`).
  final Object? message;
}

/// One KV namespace: `client.kv('cache')`.
class KvApi {
  /// Creates the API.
  KvApi(this._t, this.namespace) : _base = '/api/v1/kv/${_enc(namespace)}';

  final Transport _t;

  /// The namespace name.
  final String namespace;
  final String _base;

  /// Sets a string. The reply says whether it was set (`nx` / `xx` can
  /// refuse). [value] is a `String` or bytes.
  Future<Json> set(
    String key,
    Object value, {
    Duration? ttl,
    bool nx = false,
    bool xx = false,
    bool keepTtl = false,
  }) =>
      _t.requestJson(
        '$_base/strings/${_enc(key)}',
        method: 'PUT',
        body: {
          'value': kvEncode(value),
          if (ttl != null) 'ttl_ms': ttl.inMilliseconds,
          if (nx) 'nx': true,
          if (xx) 'xx': true,
          if (keepTtl) 'keepttl': true,
        },
      );

  /// Reads a key of any type. A missing key throws [NotFoundException]; see
  /// [getString] for a nullable read.
  Future<KvKey> get(String key) async =>
      KvKey.fromJson(await _t.requestJson('$_base/keys/${_enc(key)}'));

  /// A string key's value (`String` or `Uint8List`), `null` when missing.
  Future<Object?> getValue(String key) async {
    try {
      return kvDecode((await get(key)).value);
    } on NotFoundException {
      return null;
    }
  }

  /// A string key's value as text, `null` when missing or not UTF-8 text.
  Future<String?> getString(String key) async {
    final v = await getValue(key);
    if (v is String) return v;
    if (v is Uint8List) {
      try {
        return utf8.decode(v);
      } on FormatException {
        return null;
      }
    }
    return null;
  }

  /// Deletes a key of any type.
  Future<Json> delete(String key) =>
      _t.requestJson('$_base/keys/${_enc(key)}', method: 'DELETE');

  /// Adds [by] (default 1) to an integer string key (created as 0 when
  /// missing). The reply carries the new value.
  Future<Json> incr(String key, {int? by}) => _t.requestJson(
        '$_base/strings/${_enc(key)}/incr',
        method: 'POST',
        body: {if (by != null) 'by': by},
      );

  /// Sets the time to live of a key.
  Future<Json> expire(String key, Duration ttl) => _t.requestJson(
        '$_base/keys/${_enc(key)}/ttl',
        method: 'PUT',
        body: {'ttl_ms': ttl.inMilliseconds},
      );

  /// Removes the time to live: the key no longer expires.
  Future<Json> persist(String key) =>
      _t.requestJson('$_base/keys/${_enc(key)}/ttl', method: 'DELETE');

  /// One page of keys; start with cursor `0`, the scan is done when the
  /// returned cursor is `0` again.
  Future<({List<String> keys, String cursor})> scan({
    String cursor = '0',
    String? match,
    int? count,
    String? type,
  }) async {
    final r = await _t.requestJson(
      '$_base/keys',
      query: {'cursor': cursor, 'match': match, 'count': count, 'type': type},
    );
    return (
      keys: [for (final k in (r['keys'] as List<dynamic>? ?? const [])) '$k'],
      cursor: '${r['cursor'] ?? '0'}',
    );
  }

  /// Every key, lazily, following the scan cursor.
  Stream<String> keys({String? match, int? count, String? type}) async* {
    var cursor = '0';
    do {
      final page = await scan(
        cursor: cursor,
        match: match,
        count: count,
        type: type,
      );
      yield* Stream.fromIterable(page.keys);
      cursor = page.cursor;
    } while (cursor != '0');
  }

  /// Reads several strings at once; missing keys are `null`.
  Future<Json> mget(List<String> keys) => _t.requestJson(
        '$_base/mget',
        method: 'POST',
        body: {'keys': keys},
      );

  /// Writes several strings in one step.
  Future<Json> mset(Map<String, Object> entries) => _t.requestJson(
        '$_base/mset',
        method: 'POST',
        body: {
          'entries': [
            for (final e in entries.entries)
              {'key': e.key, 'value': kvEncode(e.value)},
          ],
        },
      );

  // ---- hashes ----

  /// Sets hash fields.
  Future<Json> hashSet(String key, Map<String, Object> fields) =>
      _t.requestJson(
        '$_base/hashes/${_enc(key)}/set',
        method: 'POST',
        body: {
          'fields': {for (final e in fields.entries) e.key: kvEncode(e.value)},
        },
      );

  /// Adds to the integer in a hash field.
  Future<Json> hashIncr(String key, String field, {int? by}) => _t.requestJson(
        '$_base/hashes/${_enc(key)}/incr',
        method: 'POST',
        body: {'field': field, if (by != null) 'by': by},
      );

  /// Removes hash fields.
  Future<Json> hashDelete(String key, List<String> fields) => _t.requestJson(
        '$_base/hashes/${_enc(key)}/delete',
        method: 'POST',
        body: {'fields': fields},
      );

  // ---- lists ----

  /// Pushes values onto the left (head) or right (tail) of a list.
  Future<Json> listPush(
    String key,
    List<Object> values, {
    bool left = false,
  }) =>
      _t.requestJson(
        '$_base/lists/${_enc(key)}/push',
        method: 'POST',
        body: {
          'side': left ? 'left' : 'right',
          'values': [for (final v in values) kvEncode(v)],
        },
      );

  /// Pops from the left or right of a list.
  Future<Json> listPop(String key, {bool left = true, int? count}) =>
      _t.requestJson(
        '$_base/lists/${_enc(key)}/pop',
        method: 'POST',
        body: {
          'side': left ? 'left' : 'right',
          if (count != null) 'count': count
        },
      );

  // ---- sets and sorted sets ----

  /// Adds set members.
  Future<Json> setAdd(String key, List<Object> members) =>
      _members('sets', key, 'add', members);

  /// Removes set members.
  Future<Json> setRemove(String key, List<Object> members) =>
      _members('sets', key, 'remove', members);

  /// Adds or re-scores sorted-set members (`member -> score`).
  Future<Json> zsetAdd(String key, Map<String, num> members) => _t.requestJson(
        '$_base/zsets/${_enc(key)}/add',
        method: 'POST',
        body: {
          'members': [
            for (final e in members.entries)
              {'member': e.key, 'score': e.value},
          ],
        },
      );

  /// Removes sorted-set members.
  Future<Json> zsetRemove(String key, List<Object> members) =>
      _members('zsets', key, 'remove', members);

  Future<Json> _members(String kind, String key, String op, List<Object> m) =>
      _t.requestJson(
        '$_base/$kind/${_enc(key)}/$op',
        method: 'POST',
        body: {
          'members': [for (final v in m) kvEncode(v)]
        },
      );

  // ---- streams ----

  /// Appends an entry to a stream; the reply carries its id.
  Future<Json> streamAdd(String key, Map<String, Object> fields,
          {int? maxlen}) =>
      _t.requestJson(
        '$_base/streams/${_enc(key)}/add',
        method: 'POST',
        body: {
          'fields': {for (final e in fields.entries) e.key: kvEncode(e.value)},
          if (maxlen != null) 'maxlen': maxlen,
        },
      );

  /// Entries of a stream between two ids (`-` and `+` are the ends).
  Future<Json> streamRange(
    String key, {
    String? start,
    String? end,
    int? count,
    bool reverse = false,
  }) =>
      _t.requestJson(
        '$_base/streams/${_enc(key)}/range',
        query: {
          'start': start,
          'end': end,
          'count': count,
          'rev': reverse ? true : null,
        },
      );

  /// Entries after an id (default `0-0`; `$` = only newer than now), without
  /// blocking.
  Future<Json> streamRead(String key, {String? after, int? count}) =>
      _t.requestJson(
        '$_base/streams/${_enc(key)}/read',
        query: {'after': after, 'count': count},
      );

  // ---- pub/sub and raw commands ----

  /// Publishes a message to a channel's current subscribers.
  Future<Json> publish(String channel, Object message) => _t.requestJson(
        '$_base/publish/${_enc(channel)}',
        method: 'POST',
        body: {'message': kvEncode(message)},
      );

  /// Subscribes to a channel as server-sent events. Messages are not stored:
  /// only subscribers connected at publish time receive them. Cancel the
  /// stream subscription to disconnect.
  Stream<KvMessage> subscribe(String channel) async* {
    final res = await _t.send(
      '$_base/subscribe/${_enc(channel)}',
      headers: {'Accept': 'text/event-stream'},
      retry: RetryOptions.none,
    );
    await for (final ev in parseSse(res.stream)) {
      if (ev.event != 'message') continue;
      final j = tryDecodeJson(ev.data);
      if (j is Map<String, dynamic>) {
        yield KvMessage(
          channel: j['channel'] as String? ?? channel,
          message: kvDecode(j['message']),
        );
      }
    }
  }

  /// Live pub/sub channels of the namespace.
  Future<Json> channels() => _t.requestJson('$_base/channels');

  /// Runs one Redis-style command (`['INCRBY', 'hits', '2']`).
  Future<Json> exec(List<Object> command) => _t.requestJson(
        '$_base/exec',
        method: 'POST',
        body: {
          'command': [for (final c in command) kvEncode(_arg(c))],
        },
      );

  /// Runs several commands in order with no other command in between. A
  /// failing command reports its error and the rest still run.
  Future<Json> batch(List<List<Object>> commands) => _t.requestJson(
        '$_base/batch',
        method: 'POST',
        body: {
          'commands': [
            for (final c in commands) [for (final a in c) kvEncode(_arg(a))],
          ],
        },
      );

  /// Deletes every key of the namespace.
  Future<Json> flush() => _t.requestJson(_base, method: 'DELETE');

  /// Engine statistics: memory, hit rate, evictions, unsaved keys.
  Future<Json> info() => _t.requestJson('/api/v1/kv/_info');

  /// Namespaces of the caller's tenant that hold keys.
  Future<Json> namespaces() => _t.requestJson('/api/v1/kv/_namespaces');
}
