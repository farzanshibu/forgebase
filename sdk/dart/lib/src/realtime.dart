import 'dart:async';
import 'dart:convert';
import 'dart:math' as math;

import 'package:web_socket_channel/web_socket_channel.dart';

import 'errors.dart';
import 'http.dart';

// ---- protocol (ADR-0011) ----------------------------------------------------

/// Something delivered to a subscription.
sealed class RealtimeEvent {
  const RealtimeEvent({required this.id});

  /// Reads a server message; `null` for types the SDK does not know.
  static RealtimeEvent? fromJson(Json m) {
    final id = m['id'] as String? ?? '';
    switch (m['type']) {
      case 'record.created' || 'record.updated' || 'record.deleted':
        return RecordEvent(
          type: switch (m['type']) {
            'record.created' => RecordEventType.created,
            'record.updated' => RecordEventType.updated,
            _ => RecordEventType.deleted,
          },
          id: id,
          cursor: (m['cursor'] as num?)?.toInt() ?? 0,
          collection: m['collection'] as String? ?? '',
          recordId: m['record_id'] as String? ?? '',
          tenantId: m['tenant_id'] as String?,
          timestamp: m['timestamp'] as String? ?? '',
          data: (m['data'] as Map<String, dynamic>?) ?? const {},
          commit: m['commit'] as String?,
        );
      case 'message':
        return ChannelMessage(
          id: id,
          channel: m['channel'] as String? ?? '',
          from: m['from'] as String? ?? '',
          data: m['data'],
          timestamp: m['timestamp'] as String? ?? '',
        );
      case 'presence.join' || 'presence.leave':
        return PresenceEvent(
          id: id,
          joined: m['type'] == 'presence.join',
          channel: m['channel'] as String? ?? '',
          member: PresenceMember.fromJson(
            (m['member'] as Map<String, dynamic>?) ?? const {},
          ),
        );
      case 'reset':
        return ResetEvent(
          id: id,
          reason: m['reason'] as String? ?? '',
          cursor: (m['cursor'] as num?)?.toInt() ?? 0,
        );
    }
    return null;
  }

  /// Subscription id.
  final String id;
}

/// Kinds of record events.
enum RecordEventType {
  /// A record was created.
  created,

  /// A record was updated.
  updated,

  /// A record was deleted.
  deleted,
}

/// A record changed.
class RecordEvent extends RealtimeEvent {
  /// Creates an event.
  const RecordEvent({
    required this.type,
    required super.id,
    required this.cursor,
    required this.collection,
    required this.recordId,
    required this.timestamp,
    required this.data,
    this.tenantId,
    this.commit,
  });

  /// What happened.
  final RecordEventType type;

  /// Resume cursor: the SDK stores the highest one per subscription.
  final int cursor;

  /// Collection name.
  final String collection;

  /// Id of the changed record.
  final String recordId;

  /// Tenant of the record.
  final String? tenantId;

  /// RFC 3339 time of the change.
  final String timestamp;

  /// The record.
  final Map<String, dynamic> data;

  /// The primary's commit position when the event's page was read
  /// (`<set>/<lsn>`); only present when the database has replicas. The client
  /// feeds it to its commit tokens.
  final String? commit;
}

/// A message on a channel.
class ChannelMessage extends RealtimeEvent {
  /// Creates a message.
  const ChannelMessage({
    required super.id,
    required this.channel,
    required this.from,
    required this.data,
    required this.timestamp,
  });

  /// Channel name.
  final String channel;

  /// Publisher.
  final String from;

  /// Payload.
  final Object? data;

  /// RFC 3339 time.
  final String timestamp;
}

/// A member of a channel's presence.
class PresenceMember {
  /// Creates a member.
  const PresenceMember({
    required this.connectionId,
    required this.actor,
    required this.joinedAt,
    this.meta,
    this.instance,
  });

  /// Reads the server's member JSON.
  factory PresenceMember.fromJson(Json j) => PresenceMember(
        connectionId: j['connection_id'] as String? ?? '',
        actor: j['actor'] as String? ?? '',
        meta: j['meta'],
        joinedAt: j['joined_at'] as String? ?? '',
        instance: j['instance'] as String?,
      );

  /// Connection id.
  final String connectionId;

  /// Who joined.
  final String actor;

  /// Public metadata.
  final Object? meta;

  /// RFC 3339 join time.
  final String joinedAt;

  /// Server instance.
  final String? instance;
}

/// A member joined or left a channel.
class PresenceEvent extends RealtimeEvent {
  /// Creates an event.
  const PresenceEvent({
    required super.id,
    required this.joined,
    required this.channel,
    required this.member,
  });

  /// `true` for `presence.join`, `false` for `presence.leave`.
  final bool joined;

  /// Channel name.
  final String channel;

  /// The member.
  final PresenceMember member;
}

/// The server could not replay from the cursor: reload your state, then
/// continue from [cursor].
class ResetEvent extends RealtimeEvent {
  /// Creates an event.
  const ResetEvent({
    required super.id,
    required this.reason,
    required this.cursor,
  });

  /// `cursor_expired`, `cursor_invalid`, `too_far_behind`, ...
  final String reason;

  /// Where the stream continues.
  final int cursor;
}

/// Receives events of a subscription.
typedef RealtimeHandler = void Function(RealtimeEvent event);

/// What to subscribe to.
class SubscribeOptions {
  /// Creates options.
  const SubscribeOptions({
    this.id,
    this.collection,
    this.record,
    this.filter,
    this.params,
    this.channel,
    this.cursor,
  });

  /// Client-chosen id (1-64 of `[A-Za-z0-9_.:-]`); generated when omitted.
  final String? id;

  /// Collection to watch (add [record] for one record, or [filter]).
  final String? collection;

  /// One record of [collection].
  final String? record;

  /// Filter-language expression the changed record must match.
  final String? filter;

  /// Values for `{:name}` placeholders in [filter].
  final Map<String, Object?>? params;

  /// Custom pub/sub channel (instead of [collection]).
  final String? channel;

  /// Replay every event after this cursor, then go live.
  final int? cursor;

  Map<String, Object?> get _spec => {
        if (collection != null) 'collection': collection,
        if (record != null) 'record': record,
        if (filter != null) 'filter': filter,
        if (params != null) 'params': params,
        if (channel != null) 'channel': channel,
      };
}

/// A live subscription.
abstract interface class Subscription {
  /// Subscription id.
  String get id;

  /// Highest cursor delivered so far (what a reconnect resumes from).
  int? get cursor;

  /// Stops the subscription.
  Future<void> unsubscribe();
}

/// Connection state.
enum RealtimeState {
  /// Never connected.
  idle,

  /// First connect in progress.
  connecting,

  /// Welcomed by the server.
  open,

  /// Lost the connection, trying again.
  reconnecting,

  /// Closed by [RealtimeClient.close] or after giving up.
  closed,
}

/// Opens a WebSocket; replace it in tests or to add platform options.
typedef WebSocketConnector = WebSocketChannel Function(Uri uri);

/// Credentials for the realtime connection.
abstract interface class TokenSource {
  /// The credential: an access token or an API key.
  Future<String?> get();

  /// Called after the server closed the socket with 4001: get a fresh
  /// credential.
  Future<void> refresh();

  /// Emits when the access token rotates while connected.
  Stream<String?> get changes;
}

/// Reconnect policy.
class ReconnectOptions {
  /// Creates a policy.
  const ReconnectOptions({
    this.baseDelay = const Duration(milliseconds: 500),
    this.maxDelay = const Duration(seconds: 15),
    this.maxAttempts,
  });

  /// First backoff delay; doubles each attempt with full jitter.
  final Duration baseDelay;

  /// Upper bound of one delay.
  final Duration maxDelay;

  /// Give up after this many consecutive failures (`null`: never).
  final int? maxAttempts;
}

class _Sub implements Subscription {
  _Sub(this.id, this.spec, this.handler, this.cursor, this._owner);

  @override
  final String id;
  final Map<String, Object?> spec;
  final RealtimeHandler handler;
  @override
  int? cursor;
  bool active = false;
  Object? error;
  final RealtimeClient _owner;

  @override
  Future<void> unsubscribe() => _owner.unsubscribe(id);
}

/// Realtime client: `client.realtime`. One connection carries every
/// subscription.
class RealtimeClient {
  /// Creates a client. [WebSocketChannel.connect] is the default connector.
  RealtimeClient({
    required Transport transport,
    required TokenSource tokens,
    WebSocketConnector? connector,
    this.reconnect = const ReconnectOptions(),
    this.requestTimeout = const Duration(seconds: 20),
    Sleeper? sleep,
    double Function()? random,
  })  : _t = transport,
        _tokens = tokens,
        _connector = connector ?? WebSocketChannel.connect,
        _sleep = sleep ?? defaultSleep,
        _random = random ?? math.Random().nextDouble;

  final Transport _t;
  final TokenSource _tokens;
  final WebSocketConnector _connector;

  /// Reconnect policy.
  final ReconnectOptions reconnect;

  /// How long a request over the socket waits for its reply.
  final Duration requestTimeout;
  final Sleeper _sleep;
  final double Function() _random;

  final Map<String, _Sub> _subs = {};
  final Map<String, Completer<Json>> _pending = {};
  final List<Completer<void>> _opened = [];
  WebSocketChannel? _ws;
  bool _wsOpen = false;
  Future<void>? _loop;
  Completer<void>? _wake;
  StreamSubscription<String?>? _tokenWatch;
  bool _stopped = false;
  int _seq = 0;
  RealtimeState _state = RealtimeState.idle;

  final _openCtl = StreamController<void>.broadcast(sync: true);
  final _closeCtl =
      StreamController<({int code, String reason})>.broadcast(sync: true);
  final _reconnectingCtl =
      StreamController<({int attempt, Duration delay})>.broadcast(sync: true);
  final _errorCtl = StreamController<Object>.broadcast(sync: true);
  final _stateCtl = StreamController<RealtimeState>.broadcast(sync: true);

  /// Current connection state.
  RealtimeState get state => _state;

  /// Emits every state change.
  Stream<RealtimeState> get stateChanges => _stateCtl.stream;

  /// Emits each time the connection is (re)established and every
  /// subscription is active again.
  Stream<void> get onOpen => _openCtl.stream;

  /// Emits when the socket closes (code, reason).
  Stream<({int code, String reason})> get onClose => _closeCtl.stream;

  /// Emits before each reconnect attempt.
  Stream<({int attempt, Duration delay})> get onReconnecting =>
      _reconnectingCtl.stream;

  /// Emits errors that do not end the connection (a failed connect, a handler
  /// that threw, a subscription the server refused after a reconnect).
  Stream<Object> get onError => _errorCtl.stream;

  void _setState(RealtimeState s) {
    if (_state == s) return;
    _state = s;
    _stateCtl.add(s);
  }

  // ---- public API -----------------------------------------------------------

  /// Opens the connection (done by the first [subscribe]). Completes once
  /// connected.
  Future<void> connect() {
    _stopped = false;
    Future<void> ready;
    if (_state == RealtimeState.open) {
      ready = Future<void>.value();
    } else {
      final c = Completer<void>();
      c.future.ignore();
      _opened.add(c);
      ready = c.future;
    }
    if (_loop == null) {
      _tokenWatch ??= _tokens.changes.listen((token) {
        if (token != null && _wsOpen) {
          _sendRaw({'type': 'auth', 'token': token});
        }
      });
      _loop = _run().whenComplete(() => _loop = null);
    }
    return ready;
  }

  /// Subscribes to a collection, a record or a channel. Completes when the
  /// server accepted the subscription.
  Future<Subscription> subscribe(
    SubscribeOptions opts,
    RealtimeHandler handler,
  ) async {
    final id =
        opts.id ?? 's${++_seq}_${(_random() * 1e6).floor().toRadixString(36)}';
    if (_subs.containsKey(id)) {
      throw RealtimeException(
        'duplicate_subscription',
        'subscription id $id is already in use',
      );
    }
    final sub = _Sub(id, opts._spec, handler, opts.cursor, this);
    _subs[id] = sub;
    try {
      if (_state == RealtimeState.open && _wsOpen) {
        await _activate(sub);
      } else {
        await connect();
        final e = sub.error;
        if (e != null) throw e;
      }
    } on Object {
      _subs.remove(id);
      rethrow;
    }
    return sub;
  }

  /// A subscription as a stream: subscribes on listen, unsubscribes on cancel.
  Stream<RealtimeEvent> watch(SubscribeOptions opts) {
    Subscription? sub;
    late final StreamController<RealtimeEvent> ctl;
    ctl = StreamController<RealtimeEvent>(
      onListen: () async {
        try {
          sub = await subscribe(opts, (e) {
            if (!ctl.isClosed) ctl.add(e);
          });
        } on Object catch (e, st) {
          if (!ctl.isClosed) ctl.addError(e, st);
        }
      },
      onCancel: () async {
        final s = sub;
        if (s != null) await s.unsubscribe();
      },
    );
    return ctl.stream;
  }

  /// Stops a subscription.
  Future<void> unsubscribe(String id) async {
    final sub = _subs.remove(id);
    if (sub == null) return;
    if (_wsOpen && sub.active) {
      try {
        await _request({'type': 'unsubscribe', 'id': id});
      } on Object {
        // The socket is going away anyway.
      }
    }
  }

  /// Publishes to a channel (over the socket when open, else REST).
  Future<void> publish(String channel, Object? data) async {
    if (_wsOpen) {
      await _request({'type': 'publish', 'channel': channel, 'data': data});
      return;
    }
    await _t.request(
      '/api/v1/realtime/channels/${Uri.encodeComponent(channel)}/messages',
      method: 'POST',
      body: {'data': data},
    );
  }

  /// Joins a channel's presence (the channel must be subscribed). Completes
  /// with the members.
  Future<List<PresenceMember>> joinPresence(String channel,
      {Object? meta}) async {
    final r = await _request({
      'type': 'presence.join',
      'channel': channel,
      if (meta != null) 'meta': meta,
    });
    return [
      for (final m in (r['members'] as List<dynamic>? ?? const []))
        PresenceMember.fromJson(m as Json),
    ];
  }

  /// Leaves a channel's presence.
  Future<void> leavePresence(String channel) async {
    await _request({'type': 'presence.leave', 'channel': channel});
  }

  /// Members currently present (REST).
  Future<List<PresenceMember>> presence(String channel) async {
    final r = await _t.requestJson(
      '/api/v1/realtime/channels/${Uri.encodeComponent(channel)}/presence',
    );
    return [
      for (final m in (r['members'] as List<dynamic>? ?? const []))
        PresenceMember.fromJson(m as Json),
    ];
  }

  /// Closes the connection and forgets every subscription.
  Future<void> close() async {
    _stopped = true;
    await _tokenWatch?.cancel();
    _tokenWatch = null;
    _subs.clear();
    _fail(RealtimeException('closed', 'realtime client closed'));
    final ws = _ws;
    _ws = null;
    _wsOpen = false;
    try {
      await ws?.sink.close(1000, 'client closed');
    } on Object {
      // Already closed.
    }
    _wake?.complete();
    _setState(RealtimeState.closed);
  }

  /// Closes the connection and the event streams for good.
  Future<void> dispose() async {
    await close();
    await _openCtl.close();
    await _closeCtl.close();
    await _reconnectingCtl.close();
    await _errorCtl.close();
    await _stateCtl.close();
  }

  // ---- connection loop ------------------------------------------------------

  void _emitError(Object e) {
    if (!_errorCtl.isClosed) _errorCtl.add(e);
  }

  Future<void> _run() async {
    var attempt = 0;
    while (!_stopped) {
      _setState(
        attempt == 0 ? RealtimeState.connecting : RealtimeState.reconnecting,
      );
      var welcomed = false;
      try {
        welcomed = await _runSocket();
      } on Object catch (e) {
        if (_stopped) break;
        _emitError(e);
      }
      if (_stopped) break;
      if (welcomed) attempt = 0;
      for (final s in _subs.values) {
        s.active = false;
      }
      attempt++;
      final max = reconnect.maxAttempts;
      if (max != null && attempt > max) {
        _fail(
          RealtimeException(
            'reconnect_failed',
            'gave up after $max reconnect attempts',
          ),
        );
        break;
      }
      final ceiling = math.min(
        reconnect.maxDelay.inMilliseconds,
        reconnect.baseDelay.inMilliseconds *
            math.pow(2, math.min(attempt - 1, 30)).toInt(),
      );
      final delay = welcomed && attempt == 1
          ? Duration.zero
          : Duration(milliseconds: (_random() * ceiling).floor());
      _setState(RealtimeState.reconnecting);
      if (!_reconnectingCtl.isClosed) {
        _reconnectingCtl.add((attempt: attempt, delay: delay));
      }
      final wake = _wake = Completer<void>();
      await Future.any<void>([_sleep(delay), wake.future]);
      _wake = null;
    }
    _setState(RealtimeState.closed);
  }

  /// Completes with whether the server welcomed us.
  Future<bool> _runSocket() async {
    final token = await _tokens.get();
    final base = Uri.parse(_t.baseUrl);
    final uri = base.replace(
      scheme: base.scheme == 'https' ? 'wss' : 'ws',
      path: '${base.path}/api/v1/realtime/ws',
      queryParameters:
          token == null || token.isEmpty ? null : {'access_token': token},
    );
    final ch = _connector(uri);
    _ws = ch;
    try {
      await ch.ready;
    } on Object catch (e) {
      if (identical(_ws, ch)) _ws = null;
      throw RealtimeException(
        'connect_failed',
        'WebSocket connection failed: $e',
      );
    }
    final done = Completer<bool>();
    var welcomed = false;
    Timer? timer;
    var last = DateTime.now();

    Future<void> finish(int code, String reason) async {
      if (done.isCompleted) return;
      timer?.cancel();
      if (identical(_ws, ch)) {
        _ws = null;
        _wsOpen = false;
      }
      _failPending(
          RealtimeException('connection_closed', 'connection closed ($code)'));
      if (!_closeCtl.isClosed) _closeCtl.add((code: code, reason: reason));
      if (code == 4001) {
        try {
          await _tokens.refresh();
        } on Object {
          // The next connect reports the failure.
        }
      }
      if (!done.isCompleted) done.complete(welcomed);
    }

    ch.stream.listen(
      (Object? data) {
        last = DateTime.now();
        if (data is! String) return;
        final Object? decoded;
        try {
          decoded = jsonDecode(data);
        } on FormatException {
          return;
        }
        if (decoded is! Map<String, dynamic>) return;
        if (decoded['type'] == 'welcome' && !welcomed) {
          welcomed = true;
          final hb = Duration(
            milliseconds: ((decoded['heartbeat_ms'] as num?)?.toInt() ?? 0) > 0
                ? (decoded['heartbeat_ms'] as num).toInt()
                : 25000,
          );
          timer = Timer.periodic(hb, (_) {
            if (DateTime.now().difference(last) > hb * 2.5) {
              unawaited(_closeQuietly(ch, 4002, 'heartbeat timeout'));
            } else if (_wsOpen) {
              _sendRaw({'type': 'ping'});
            }
          });
          _wsOpen = true;
          _setState(RealtimeState.open);
          unawaited(
            _resubscribeAll().then((_) {
              if (!_openCtl.isClosed) _openCtl.add(null);
              _settleOpened();
            }),
          );
          return;
        }
        _onMessage(decoded);
      },
      onError: (Object e) {
        _emitError(e);
        if (!welcomed) {
          unawaited(finish(1006, 'connect failed'));
        }
      },
      onDone: () =>
          unawaited(finish(ch.closeCode ?? 1006, ch.closeReason ?? '')),
    );
    return done.future;
  }

  Future<void> _closeQuietly(
      WebSocketChannel ch, int code, String reason) async {
    try {
      await ch.sink.close(code, reason);
    } on Object {
      // Already closed.
    }
  }

  Future<void> _resubscribeAll() async {
    await Future.wait([
      for (final s in _subs.values.toList())
        _activate(s).catchError((Object err) {
          // A subscription the server now refuses (rule changed while
          // offline) is dropped and reported.
          s.error = err;
          _subs.remove(s.id);
          _emitError(err);
        }),
    ]);
  }

  Future<void> _activate(_Sub sub) async {
    final r = await _request({
      'type': 'subscribe',
      'id': sub.id,
      ...sub.spec,
      if (sub.cursor != null) 'cursor': sub.cursor,
    });
    sub.active = true;
    final c = r['cursor'];
    if (c is num && sub.cursor == null) sub.cursor = c.toInt();
  }

  void _sendRaw(Map<String, Object?> msg) {
    try {
      _ws?.sink.add(jsonEncode(msg));
    } on Object {
      // A dead socket is noticed by the loop.
    }
  }

  Future<Json> _request(Map<String, Object?> msg) {
    final ws = _ws;
    if (ws == null || !_wsOpen) {
      return Future.error(
        RealtimeException('not_connected', 'realtime socket is not open'),
      );
    }
    final ref = '${++_seq}';
    final c = Completer<Json>();
    _pending[ref] = c;
    ws.sink.add(jsonEncode({...msg, 'ref': ref}));
    return c.future.timeout(
      requestTimeout,
      onTimeout: () {
        _pending.remove(ref);
        throw RealtimeException('timeout', 'no reply to ${msg['type']}');
      },
    );
  }

  void _onMessage(Json m) {
    final r = m['ref'];
    final ref = r is String ? r : (r is num ? '${r.toInt()}' : null);
    if (ref != null && _pending.containsKey(ref)) {
      final p = _pending.remove(ref)!;
      if (m['type'] == 'error') {
        p.completeError(
          RealtimeException(
            '${m['code'] ?? 'error'}',
            '${m['message'] ?? 'realtime error'}',
          ),
        );
      } else {
        p.complete(m);
      }
      return;
    }
    final type = m['type'];
    if (type == 'error') {
      _emitError(
        RealtimeException(
          '${m['code'] ?? 'error'}',
          '${m['message'] ?? 'realtime error'}',
        ),
      );
      return;
    }
    final sub = m['id'] is String ? _subs[m['id']] : null;
    if (sub == null) return;
    final event = RealtimeEvent.fromJson(m);
    if (event == null) return;
    if (m['commit'] is String) _t.commitTokens.note(m['commit'] as String);
    final cursor = m['cursor'];
    if (cursor is num &&
        (sub.cursor == null || cursor > sub.cursor! || type == 'reset')) {
      sub.cursor = cursor.toInt();
    }
    try {
      sub.handler(event);
    } on Object catch (e) {
      _emitError(e);
    }
  }

  void _settleOpened() {
    final w = List<Completer<void>>.of(_opened);
    _opened.clear();
    for (final c in w) {
      if (!c.isCompleted) c.complete();
    }
  }

  void _failPending(Object e) {
    final p = List<Completer<Json>>.of(_pending.values);
    _pending.clear();
    for (final c in p) {
      if (!c.isCompleted) c.completeError(e);
    }
  }

  void _fail(Object e) {
    _failPending(e);
    final w = List<Completer<void>>.of(_opened);
    _opened.clear();
    for (final c in w) {
      if (!c.isCompleted) c.completeError(e);
    }
  }
}
