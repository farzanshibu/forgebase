import 'dart:async';

import 'package:forgebase/forgebase.dart';
import 'package:test/test.dart';

import 'helpers.dart';

Map<String, dynamic> recordEvent(
  String sub,
  int cursor, {
  String type = 'record.created',
  String? commit,
}) =>
    {
      'type': type,
      'id': sub,
      'cursor': cursor,
      'collection': 'posts',
      'record_id': 'r$cursor',
      'timestamp': '2026-01-01T00:00:00Z',
      'data': {'id': 'r$cursor'},
      if (commit != null) 'commit': commit,
    };

void main() {
  late WsHub hub;

  Harness make({String? apiKey, String? accessToken, Handler? handler}) {
    hub = WsHub();
    return Harness(
      handler ?? (r) => ok({}),
      apiKey: apiKey,
      accessToken: accessToken,
      ws: hub.connect,
    );
  }

  test('connects with the access token in the query and subscribes', () async {
    final h = make(accessToken: 'tok');
    final events = <RealtimeEvent>[];
    final sub = await h.fb.realtime.subscribe(
      const SubscribeOptions(id: 's1', collection: 'posts', filter: 'x = 1'),
      events.add,
    );
    expect(hub.last.uri.toString(),
        'ws://fb.test/api/v1/realtime/ws?access_token=tok');
    expect(hub.last.sentOfType('subscribe').single, {
      'type': 'subscribe',
      'id': 's1',
      'collection': 'posts',
      'filter': 'x = 1',
      'ref': hub.last.sentOfType('subscribe').single['ref'],
    });
    expect(sub.id, 's1');
    expect(h.fb.realtime.state, RealtimeState.open);

    hub.last.serverSend(recordEvent('s1', 4));
    await waitFor(() => events.isNotEmpty);
    final e = events.single as RecordEvent;
    expect(e.type, RecordEventType.created);
    expect(e.recordId, 'r4');
    expect(e.data['id'], 'r4');
    expect(sub.cursor, 4);
    await h.fb.close();
  });

  test('https base uses wss and an API key rides as the token', () async {
    hub = WsHub();
    final server = FakeServer((r) => ok({}));
    final fb = ForgeBase(
      'https://fb.example.com/base',
      apiKey: 'fb_key',
      httpClient: server.client,
      webSocketConnector: hub.connect,
    );
    await fb.realtime.connect();
    expect(hub.last.uri.toString(),
        'wss://fb.example.com/base/api/v1/realtime/ws?access_token=fb_key');
    await fb.close();
  });

  test('no credential means no access_token parameter', () async {
    final h = make();
    await h.fb.realtime.connect();
    expect(hub.last.uri.query, isEmpty);
    await h.fb.close();
  });

  test('tracks the highest cursor, feeds commit tokens, delivers every kind',
      () async {
    final h = make();
    final events = <RealtimeEvent>[];
    final sub = await h.fb.realtime.subscribe(
      const SubscribeOptions(id: 's1', collection: 'posts'),
      events.add,
    );
    final ws = hub.last;
    ws.serverSend(recordEvent('s1', 5, commit: 'control/1/A'));
    ws.serverSend(recordEvent('s1', 3, type: 'record.updated'));
    ws.serverSend({
      'type': 'message',
      'id': 's1',
      'channel': 'room',
      'from': 'u1',
      'data': {'hi': 1},
      'timestamp': 't',
    });
    ws.serverSend({
      'type': 'presence.join',
      'id': 's1',
      'channel': 'room',
      'member': {'connection_id': 'c', 'actor': 'u2', 'joined_at': 't'},
    });
    ws.serverSend(
        {'type': 'reset', 'id': 's1', 'reason': 'cursor_expired', 'cursor': 1});
    ws.serverSend(recordEvent('other', 9));
    await waitFor(() => events.length == 5);
    expect(events[0], isA<RecordEvent>());
    expect((events[1] as RecordEvent).type, RecordEventType.updated);
    expect((events[2] as ChannelMessage).data, {'hi': 1});
    expect((events[3] as PresenceEvent).joined, isTrue);
    expect((events[4] as ResetEvent).reason, 'cursor_expired');
    // 5, then 3 is lower and ignored, then reset moves it back to 1.
    expect(sub.cursor, 1);
    expect(h.fb.commitTokens.get('control'), '1/A');
    await h.fb.close();
  });

  test('a throwing handler is reported and does not break the stream',
      () async {
    final h = make();
    final errors = <Object>[];
    h.fb.realtime.onError.listen(errors.add);
    final got = <int>[];
    await h.fb.realtime.subscribe(
      const SubscribeOptions(id: 's1', collection: 'posts'),
      (e) {
        got.add((e as RecordEvent).cursor);
        if (got.length == 1) throw StateError('handler bug');
      },
    );
    hub.last.serverSend(recordEvent('s1', 1));
    hub.last.serverSend(recordEvent('s1', 2));
    await waitFor(() => got.length == 2);
    expect(errors.single, isA<StateError>());
    await h.fb.close();
  });

  test('reconnects and resumes every subscription from its cursor', () async {
    final h = make();
    final reconnects = <int>[];
    h.fb.realtime.onReconnecting.listen((e) => reconnects.add(e.attempt));
    final got = <int>[];
    await h.fb.realtime.subscribe(
      const SubscribeOptions(id: 's1', collection: 'posts'),
      (e) => got.add((e as RecordEvent).cursor),
    );
    final first = hub.last;
    first.serverSend(recordEvent('s1', 7));
    await waitFor(() => got.isNotEmpty);

    final opened = Completer<void>();
    final sub = h.fb.realtime.onOpen.listen((_) => opened.complete());
    first.serverClose(1006, 'lost');
    await opened.future.timeout(const Duration(seconds: 2));
    await sub.cancel();

    expect(hub.sockets.length, 2);
    final resub = hub.last.sentOfType('subscribe').single;
    expect(resub['id'], 's1');
    expect(resub['cursor'], 7, reason: 'resume from the last cursor');
    expect(reconnects, isNotEmpty);

    hub.last.serverSend(recordEvent('s1', 8));
    await waitFor(() => got.length == 2);
    expect(got, [7, 8]);
    await h.fb.close();
  });

  test('keeps retrying with backoff while the server is down', () async {
    final h = make();
    hub.failNextConnects = 3;
    final errors = <Object>[];
    h.fb.realtime.onError.listen(errors.add);
    await h.fb.realtime.subscribe(
      const SubscribeOptions(id: 's1', collection: 'posts'),
      (_) {},
    );
    expect(hub.sockets.length, 4);
    expect(errors.length, 3);
    expect(errors.first, isA<RealtimeException>());
    expect((errors.first as RealtimeException).code, 'connect_failed');
    await h.fb.close();
  });

  test('gives up after maxAttempts', () async {
    hub = WsHub()..failNextConnects = 100;
    final server = FakeServer((r) => ok({}));
    final fb = ForgeBase(
      base,
      httpClient: server.client,
      webSocketConnector: hub.connect,
      reconnect: const ReconnectOptions(
        baseDelay: Duration(milliseconds: 1),
        maxDelay: Duration(milliseconds: 2),
        maxAttempts: 2,
      ),
    );
    await expectLater(
      fb.realtime.connect(),
      throwsA(
        isA<RealtimeException>()
            .having((e) => e.code, 'code', 'reconnect_failed'),
      ),
    );
    expect(fb.realtime.state, RealtimeState.closed);
    await fb.close();
  });

  test('close code 4001 refreshes the token before reconnecting', () async {
    var refreshed = 0;
    final h = make(
      handler: (r) {
        if (r.path == '/api/auth/login') return ok(wireSession());
        if (r.path == '/api/auth/refresh') {
          refreshed++;
          return ok(wireSession(access: 'at2', refresh: 'rt2'));
        }
        return ok({});
      },
    );
    await h.fb.auth.signIn(email: 'a', password: 'b');
    await h.fb.realtime.connect();
    expect(hub.last.uri.queryParameters['access_token'], 'at1');
    final opened = Completer<void>();
    final s = h.fb.realtime.onOpen.listen((_) => opened.complete());
    hub.last.serverClose(4001, 'expired');
    await opened.future.timeout(const Duration(seconds: 2));
    await s.cancel();
    expect(refreshed, 1);
    expect(hub.last.uri.queryParameters['access_token'], 'at2');
    await h.fb.close();
  });

  test('a rotated access token is pushed over the open socket', () async {
    final h = make(
      handler: (r) {
        if (r.path == '/api/auth/login') return ok(wireSession());
        return ok(wireSession(access: 'at2', refresh: 'rt2'));
      },
    );
    await h.fb.auth.signIn(email: 'a', password: 'b');
    await h.fb.realtime.connect();
    await h.fb.auth.refresh();
    await waitFor(() => hub.last.sentOfType('auth').isNotEmpty);
    expect(hub.last.sentOfType('auth').single['token'], 'at2');
    await h.fb.close();
  });

  test('a refused subscription fails subscribe with the server error',
      () async {
    final h = make();
    hub.reply = (ws, m) => m['type'] == 'subscribe'
        ? {'type': 'error', 'code': 'forbidden', 'message': 'no access'}
        : null;
    await expectLater(
      h.fb.realtime.subscribe(
        const SubscribeOptions(id: 's1', collection: 'secret'),
        (_) {},
      ),
      throwsA(
        isA<RealtimeException>()
            .having((e) => e.code, 'code', 'forbidden')
            .having((e) => e.message, 'message', 'no access'),
      ),
    );
    // The id is free again.
    hub.reply = null;
    await h.fb.realtime.subscribe(
      const SubscribeOptions(id: 's1', collection: 'posts'),
      (_) {},
    );
    await h.fb.close();
  });

  test('duplicate subscription ids are rejected', () async {
    final h = make();
    await h.fb.realtime.subscribe(
      const SubscribeOptions(id: 'dup', collection: 'a'),
      (_) {},
    );
    await expectLater(
      h.fb.realtime.subscribe(
        const SubscribeOptions(id: 'dup', collection: 'b'),
        (_) {},
      ),
      throwsA(isA<RealtimeException>()),
    );
    await h.fb.close();
  });

  test('unsubscribe tells the server and stops delivery', () async {
    final h = make();
    final got = <RealtimeEvent>[];
    final sub = await h.fb.realtime.subscribe(
      const SubscribeOptions(id: 's1', channel: 'room'),
      got.add,
    );
    await sub.unsubscribe();
    expect(hub.last.sentOfType('unsubscribe').single['id'], 's1');
    hub.last.serverSend(recordEvent('s1', 1));
    await Future<void>.delayed(const Duration(milliseconds: 20));
    expect(got, isEmpty);
    await h.fb.close();
  });

  test('publish and presence go over the socket', () async {
    final h = make();
    hub.reply = (ws, m) => switch (m['type']) {
          'presence.join' => {
              'type': 'presence',
              'members': [
                {
                  'connection_id': 'c1',
                  'actor': 'u1',
                  'joined_at': 't',
                  'meta': {'x': 1}
                },
              ],
            },
          _ => null,
        };
    await h.fb.realtime.subscribe(
      const SubscribeOptions(id: 's1', channel: 'room'),
      (_) {},
    );
    await h.fb.realtime.publish('room', {'text': 'hi'});
    expect(hub.last.sentOfType('publish').single['data'], {'text': 'hi'});
    final members = await h.fb.realtime.joinPresence('room', meta: {'x': 1});
    expect(members.single.actor, 'u1');
    expect(members.single.meta, {'x': 1});
    await h.fb.realtime.leavePresence('room');
    expect(hub.last.sentOfType('presence.leave'), hasLength(1));
    await h.fb.close();
  });

  test('publish without a socket falls back to REST; presence list is REST',
      () async {
    final h = make(
      handler: (r) => r.path.endsWith('/presence')
          ? ok({
              'members': [
                {'connection_id': 'c', 'actor': 'u', 'joined_at': 't'},
              ],
            })
          : Reply(204),
    );
    await h.fb.realtime.publish('room', {'a': 1});
    expect(h.server.last.method, 'POST');
    expect(h.server.last.path, '/api/v1/realtime/channels/room/messages');
    expect(h.server.last.map, {
      'data': {'a': 1},
    });
    expect((await h.fb.realtime.presence('room')).single.actor, 'u');
    expect(hub.sockets, isEmpty);
    await h.fb.close();
  });

  test('requests fail when the connection drops before the reply', () async {
    final h = make();
    await h.fb.realtime.subscribe(
      const SubscribeOptions(id: 's1', channel: 'room'),
      (_) {},
    );
    hub.reply = (ws, m) {
      if (m['type'] == 'publish') ws.serverClose(1006, 'gone');
      return null;
    };
    await expectLater(
      h.fb.realtime.publish('room', 1),
      throwsA(isA<RealtimeException>()),
    );
    await h.fb.close();
  });

  test('a silent server is closed with 4002 after the heartbeat window',
      () async {
    hub = WsHub(heartbeatMs: 30);
    final server = FakeServer((r) => ok({}));
    final fb = ForgeBase(
      base,
      httpClient: server.client,
      webSocketConnector: hub.connect,
      reconnect: const ReconnectOptions(
        baseDelay: Duration(milliseconds: 1),
        maxDelay: Duration(milliseconds: 2),
      ),
    );
    await fb.realtime.connect();
    final first = hub.last;
    // Swallow pings so the connection looks dead.
    first.onClient = null;
    final closed = fb.realtime.onClose.first;
    final c = await closed.timeout(const Duration(seconds: 2));
    expect(c.code, 4002);
    expect(first.sentOfType('ping'), isNotEmpty);
    await fb.close();
  });

  test('watch exposes a subscription as a stream', () async {
    final h = make();
    final stream = h.fb.realtime.watch(
      const SubscribeOptions(id: 'w1', collection: 'posts'),
    );
    final got = <RealtimeEvent>[];
    final sub = stream.listen(got.add);
    await waitFor(() =>
        hub.sockets.isNotEmpty && hub.last.sentOfType('subscribe').isNotEmpty);
    await waitFor(() => h.fb.realtime.state == RealtimeState.open);
    await Future<void>.delayed(const Duration(milliseconds: 10));
    hub.last.serverSend(recordEvent('w1', 1));
    await waitFor(() => got.isNotEmpty);
    await sub.cancel();
    await waitFor(() => hub.last.sentOfType('unsubscribe').isNotEmpty);
    await h.fb.close();
  });

  test('close fails pending connects and stops reconnecting', () async {
    final h = make();
    hub.autoWelcome = false;
    final f = h.fb.realtime.connect();
    final failed = expectLater(f, throwsA(isA<RealtimeException>()));
    await waitFor(() => hub.sockets.isNotEmpty);
    await h.fb.realtime.close();
    await failed;
    expect(h.fb.realtime.state, RealtimeState.closed);
    final n = hub.sockets.length;
    await Future<void>.delayed(const Duration(milliseconds: 30));
    expect(hub.sockets.length, n);
    await h.fb.close();
  });
}
