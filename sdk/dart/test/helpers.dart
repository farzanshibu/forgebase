import 'dart:async';
import 'dart:convert';

import 'package:forgebase/forgebase.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:stream_channel/stream_channel.dart';
import 'package:web_socket_channel/web_socket_channel.dart';

const base = 'http://fb.test';

/// A request the fake server saw.
class Rec {
  Rec(this.method, this.url, this.headers, this.bodyBytes);

  final String method;
  final Uri url;
  final Map<String, String> headers;
  final List<int> bodyBytes;

  String get path => url.path;
  String get body => utf8.decode(bodyBytes);
  Object? get json => body.isEmpty ? null : jsonDecode(body);
  Map<String, dynamic> get map => json! as Map<String, dynamic>;
  Map<String, String> get query => url.queryParameters;

  @override
  String toString() => '$method $url';
}

/// What the fake server answers.
class Reply {
  Reply(
    this.status, {
    Object? json,
    String? text,
    List<int>? bytes,
    this.stream,
    Map<String, String>? headers,
  })  : headers = {
          if (json != null) 'content-type': 'application/json',
          ...?headers,
        },
        bytes = bytes ??
            utf8.encode(text ?? (json == null ? '' : jsonEncode(json)));

  final int status;
  final List<int> bytes;
  final Stream<List<int>>? stream;
  final Map<String, String> headers;
}

Reply ok(Object json, {Map<String, String>? headers}) =>
    Reply(200, json: json, headers: headers);

Reply apiError(
  int status,
  String code,
  String message, {
  Map<String, String>? headers,
  Map<String, dynamic>? details,
}) =>
    Reply(
      status,
      json: {
        'error': {
          'code': code,
          'message': message,
          'request_id': 'req_1',
          if (details != null) 'details': details,
        },
      },
      headers: headers,
    );

typedef Handler = FutureOr<Reply> Function(Rec r);

/// An `http.Client` that records requests and answers with [handler].
class FakeServer {
  FakeServer(this.handler) {
    client = MockClient.streaming((req, body) async {
      final bytes = await body.expand((c) => c).toList();
      final rec = Rec(req.method, req.url, Map.of(req.headers), bytes);
      log.add(rec);
      final r = await handler(rec);
      return http.StreamedResponse(
        r.stream ?? Stream.value(r.bytes),
        r.status,
        headers: r.headers,
        contentLength: int.tryParse(r.headers['content-length'] ?? ''),
        request: req,
      );
    });
  }

  Handler handler;
  final List<Rec> log = [];
  late final http.Client client;

  Rec get last => log.last;
  List<Rec> where(String method, String path) =>
      log.where((r) => r.method == method && r.path == path).toList();
}

/// A client wired to a fake server, with instant sleeps.
class Harness {
  Harness(
    Handler handler, {
    String? apiKey,
    String? accessToken,
    TokenStorage? storage,
    WebSocketConnector? ws,
    Consistency consistency = Consistency.session,
    DateTime Function()? now,
    RetryOptions retry = const RetryOptions(),
    ReconnectOptions reconnect = const ReconnectOptions(
      baseDelay: Duration(milliseconds: 1),
      maxDelay: Duration(milliseconds: 2),
    ),
  }) : server = FakeServer(handler) {
    fb = ForgeBase(
      base,
      apiKey: apiKey,
      accessToken: accessToken,
      storage: storage,
      httpClient: server.client,
      webSocketConnector: ws,
      consistency: consistency,
      retry: retry,
      reconnect: reconnect,
      now: now,
      sleep: (d) async {
        sleeps.add(d);
        await Future<void>.delayed(Duration.zero);
      },
      random: () => 0.5,
    );
  }

  final FakeServer server;
  late final ForgeBase fb;
  final List<Duration> sleeps = [];
}

Map<String, dynamic> wireSession({
  String access = 'at1',
  String refresh = 'rt1',
  int expiresIn = 900,
  String id = 'sess1',
}) =>
    {
      'token_type': 'Bearer',
      'access_token': access,
      'refresh_token': refresh,
      'expires_in': expiresIn,
      'session_id': id,
      'session_expires_at': '2030-01-01T00:00:00Z',
      'subject': {'kind': 'user', 'collection': 'users', 'id': 'u1'},
      'user': {'id': 'u1', 'email': 'a@b.c'},
    };

// ---- fake WebSocket --------------------------------------------------------

class _FakeSink implements WebSocketSink {
  _FakeSink(this._ws);
  final FakeWs _ws;

  @override
  void add(dynamic data) {
    if (_ws.closeCode != null) return;
    final m = jsonDecode(data as String) as Map<String, dynamic>;
    _ws.sent.add(m);
    _ws.onClient?.call(_ws, m);
  }

  @override
  void addError(Object error, [StackTrace? stackTrace]) {}

  @override
  Future<void> addStream(Stream<dynamic> stream) => stream.forEach(add);

  @override
  Future<void> close([int? closeCode, String? closeReason]) async {
    _ws.serverClose(closeCode ?? 1005, closeReason ?? '');
  }

  @override
  Future<void> get done => _ws._done.future;
}

/// A WebSocket the test drives by hand.
class FakeWs extends StreamChannelMixin<dynamic> implements WebSocketChannel {
  FakeWs(this.uri);

  final Uri uri;
  final List<Map<String, dynamic>> sent = [];
  void Function(FakeWs ws, Map<String, dynamic> msg)? onClient;
  final _in = StreamController<dynamic>();
  final _done = Completer<void>();
  Object? failConnect;
  int? _code;
  String? _reason;

  @override
  Stream<dynamic> get stream => _in.stream;

  @override
  late final WebSocketSink sink = _FakeSink(this);

  @override
  String? get protocol => null;

  @override
  int? get closeCode => _code;

  @override
  String? get closeReason => _reason;

  @override
  Future<void> get ready async {
    if (failConnect != null) throw failConnect!;
  }

  void serverSend(Map<String, dynamic> m) {
    if (_code == null) _in.add(jsonEncode(m));
  }

  void serverClose([int code = 1006, String reason = '']) {
    if (_code != null) return;
    _code = code;
    _reason = reason;
    if (!_done.isCompleted) _done.complete();
    unawaited(_in.close());
  }

  List<Map<String, dynamic>> sentOfType(String type) =>
      sent.where((m) => m['type'] == type).toList();
}

/// Hands out [FakeWs] sockets and answers the protocol like the server does.
class WsHub {
  WsHub({this.heartbeatMs = 25000});

  final List<FakeWs> sockets = [];
  final int heartbeatMs;
  bool autoWelcome = true;
  int failNextConnects = 0;
  Map<String, dynamic>? Function(FakeWs ws, Map<String, dynamic> msg)? reply;

  FakeWs get last => sockets.last;

  WebSocketChannel connect(Uri uri) {
    final ws = FakeWs(uri);
    sockets.add(ws);
    if (failNextConnects > 0) {
      failNextConnects--;
      ws.failConnect = StateError('connection refused');
      return ws;
    }
    ws.onClient = (w, m) {
      final custom = reply?.call(w, m);
      final r = custom ??
          switch (m['type']) {
            'subscribe' => {'type': 'subscribed', 'id': m['id']},
            'unsubscribe' => {'type': 'unsubscribed', 'id': m['id']},
            'publish' => {'type': 'published'},
            'presence.join' => {'type': 'presence', 'members': <dynamic>[]},
            'presence.leave' => {'type': 'presence'},
            'auth' => {'type': 'authenticated'},
            'ping' => {'type': 'pong'},
            _ => null,
          };
      if (r != null) {
        scheduleMicrotask(() => w.serverSend({...r, 'ref': m['ref']}));
      }
    };
    if (autoWelcome) {
      scheduleMicrotask(() {
        ws.serverSend({
          'type': 'welcome',
          'connection_id': 'c${sockets.length}',
          'heartbeat_ms': heartbeatMs,
        });
      });
    }
    return ws;
  }
}

/// Polls until [cond] holds.
Future<void> waitFor(bool Function() cond, {int ms = 2000}) async {
  final end = DateTime.now().add(Duration(milliseconds: ms));
  while (!cond()) {
    if (DateTime.now().isAfter(end)) throw StateError('condition not met');
    await Future<void>.delayed(const Duration(milliseconds: 2));
  }
}
