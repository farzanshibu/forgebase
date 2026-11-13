import 'dart:async';
import 'dart:convert';

import 'package:forgebase/forgebase.dart';
import 'package:test/test.dart';

import 'helpers.dart';

Map<String, dynamic> received(String ackId, Object? data, {int attempt = 1}) =>
    {
      'ack_id': ackId,
      'delivery_attempt': attempt,
      'ack_deadline': '2026-01-01T00:00:30Z',
      'message': {
        'id': 'm-$ackId',
        'topic': 't',
        'seq': 1,
        'data': data,
        'attributes': {'kind': 'a'},
      },
    };

String sse(String event, Object data) =>
    'event: $event\ndata: ${jsonEncode(data)}\n\n';

void main() {
  test('publish sends one message and returns the result', () async {
    final h = Harness(
      (r) => ok({
        'results': [
          {'message_id': 'm1', 'seq': 4, 'duplicate': false},
        ],
      }),
    );
    final p = await h.fb.queues.publish(
      'orders',
      {'n': 1},
      attributes: {'kind': 'rush'},
      orderingKey: 'u1',
    );
    expect(p.messageId, 'm1');
    expect(p.seq, 4);
    expect(p.duplicate, isFalse);
    expect(h.server.last.path, '/api/v1/queues/topics/orders/publish');
    expect(h.server.last.map, {
      'messages': [
        {
          'data': {'n': 1},
          'attributes': {'kind': 'rush'},
          'ordering_key': 'u1',
        },
      ],
    });
  });

  test('a publish is retried on 503 only when every message has a dedup id',
      () async {
    var n = 0;
    final h = Harness(
      (r) => ++n < 2
          ? apiError(503, 'unavailable', 'x')
          : ok({'results': <Map<String, dynamic>>[]}),
    );
    await h.fb.queues
        .publishBatch('t', [const PublishMessage(1, dedupId: 'a')]);
    expect(n, 2);

    final h2 = Harness((r) => apiError(503, 'unavailable', 'x'));
    await expectLater(
      h2.fb.queues.publish('t', 1),
      throwsA(isA<ServerException>()),
    );
    expect(h2.server.log.length, 1, reason: 'a blind retry could duplicate');
    final h3 = Harness((r) => apiError(503, 'unavailable', 'x'));
    await expectLater(
      h3.fb.queues.publishBatch('t', [
        const PublishMessage(1, dedupId: 'a'),
        const PublishMessage(2),
      ]),
      throwsA(isA<ServerException>()),
    );
    expect(h3.server.log.length, 1);
  });

  test('publishBatch preserves order of results', () async {
    final h = Harness(
      (r) => ok({
        'results': [
          {'message_id': 'a', 'seq': 1, 'duplicate': false},
          {'message_id': 'a', 'seq': 1, 'duplicate': true},
        ],
      }),
    );
    final r = await h.fb.queues.publishBatch('t', [
      const PublishMessage(1, dedupId: 'x'),
      const PublishMessage(1, dedupId: 'x'),
    ]);
    expect(r.map((p) => p.duplicate), [false, true]);
  });

  test('pull, ack, nack, modifyDeadline', () async {
    final h = Harness((r) {
      if (r.path.endsWith('/pull')) {
        return ok({
          'messages': [
            received('a1', {'x': 1})
          ],
        });
      }
      return ok({
        'count': 1,
        'unknown': ['zz']
      });
    });
    final msgs =
        await h.fb.queues.pull('s', maxMessages: 5, waitMs: 100, consumer: 'c');
    expect(h.server.last.map,
        {'max_messages': 5, 'wait_ms': 100, 'consumer': 'c'});
    expect(msgs.single.message.data, {'x': 1});
    expect(msgs.single.message.attributes, {'kind': 'a'});
    expect(msgs.single.deliveryAttempt, 1);

    final a = await h.fb.queues.ack('s', ['a1']);
    expect(a.count, 1);
    expect(a.unknown, ['zz']);
    expect(h.server.last.map, {
      'ack_ids': ['a1']
    });
    await h.fb.queues.nack('s', ['a1']);
    expect(h.server.last.path, '/api/v1/queues/subscriptions/s/nack');
    await h.fb.queues.modifyDeadline('s', ['a1'], const Duration(seconds: 30));
    expect(h.server.last.map, {
      'ack_ids': ['a1'],
      'ack_deadline_ms': 30000
    });
  });

  test('pull is never retried', () async {
    final h = Harness((r) => apiError(503, 'x', 'x'));
    await expectLater(h.fb.queues.pull('s'), throwsA(isA<ServerException>()));
    expect(h.server.log.length, 1);
  });

  test('topic and subscription management', () async {
    final h = Harness((r) {
      if (r.method == 'DELETE') return Reply(204);
      if (r.path.endsWith('/topics') && r.method == 'GET') {
        return ok({
          'items': [
            {'name': 'a'}
          ]
        });
      }
      return ok({'name': 'x'});
    });
    final q = h.fb.queues;
    expect((await q.listTopics()).single['name'], 'a');
    await q.createTopic('orders', retentionMs: 1000, labels: {'a': 'b'});
    expect(h.server.last.map, {
      'name': 'orders',
      'retention_ms': 1000,
      'labels': {'a': 'b'},
    });
    await q.createSubscription(
      'worker',
      'orders',
      filter: {'kind': 'rush'},
      ackDeadlineMs: 5000,
      ordering: true,
      start: 'earliest',
    );
    expect(h.server.last.map, {
      'name': 'worker',
      'topic': 'orders',
      'filter': {'kind': 'rush'},
      'ack_deadline_ms': 5000,
      'ordering': true,
      'start': 'earliest',
    });
    await q.deleteTopic('orders', force: true);
    expect(h.server.last.query, {'force': 'true'});
    await q.deleteSubscription('worker');
    await q.seek('worker', time: DateTime.utc(2026, 1, 1));
    expect(h.server.last.map, {'time': '2026-01-01T00:00:00.000Z'});
    await q.seek('worker', snapshot: 'snap');
    expect(h.server.last.map, {'snapshot': 'snap'});
    expect(() => q.seek('worker'), throwsArgumentError);
    await q.createSnapshot('snap', 'worker');
    await q.deleteSnapshot('snap');
    final m = await q.messages('orders', after: 3, limit: 2);
    expect(h.server.last.query, {'after': '3', 'limit': '2'});
    expect(m.nextAfter, isNull);
  });

  group('streaming pull', () {
    test('acks on success, nacks on failure, reconnects after the stream ends',
        () async {
      var connects = 0;
      final ackLog = <String>[];
      final h = Harness((r) {
        if (r.path.endsWith('/events')) {
          connects++;
          if (connects == 1) {
            expect(r.headers['Accept'], 'text/event-stream');
            expect(r.query, {'max_messages': '10'});
            return Reply(
              200,
              text: ': keepalive\n\n${sse('message', received('a1', 'ok'))}'
                  '${sse('message', received('a2', 'bad'))}',
              headers: {'content-type': 'text/event-stream'},
            );
          }
          // Second connection stays open until the test unsubscribes.
          return Reply(
            200,
            stream: StreamController<List<int>>().stream,
            headers: {'content-type': 'text/event-stream'},
          );
        }
        ackLog.add('${r.path.split('/').last}:${r.map['ack_ids']}');
        return ok({'count': 1, 'unknown': <String>[]});
      });
      final errors = <Object>[];
      final seen = <Object?>[];
      final s = h.fb.queues.subscribe(
        'worker',
        (d) {
          seen.add(d.data);
          if (d.data == 'bad') throw StateError('nope');
        },
        maxMessages: 10,
        onError: errors.add,
      );
      await waitFor(() => connects == 2);
      await waitFor(() => ackLog.length == 2);
      expect(seen, ['ok', 'bad']);
      expect(ackLog, unorderedEquals(['ack:[a1]', 'nack:[a2]']));
      expect(errors.whereType<StateError>(), hasLength(1));
      s.unsubscribe();
      await s.done;
    });

    test('autoAck off leaves settling to the handler', () async {
      final ackLog = <String>[];
      final h = Harness((r) {
        if (r.path.endsWith('/events')) {
          return Reply(
            200,
            text: sse('message', received('a1', 1)),
            headers: {'content-type': 'text/event-stream'},
          );
        }
        ackLog.add(r.path.split('/').last);
        return ok({'count': 1, 'unknown': <String>[]});
      });
      final s = h.fb.queues.subscribe(
        'w',
        (d) async {
          await d.extend(const Duration(seconds: 20));
          await d.ack();
          await d.ack(); // a second settle is a no-op
        },
        autoAck: false,
        reconnect: false,
      );
      await s.done;
      expect(ackLog, ['modify-deadline', 'ack']);
    });

    test('a refusal ends the stream and fails done', () async {
      final h = Harness((r) => apiError(403, 'forbidden', 'no'));
      final s = h.fb.queues.subscribe('w', (d) {});
      await expectLater(s.done, throwsA(isA<PermissionException>()));
    });

    test('an error event ends the stream with its code', () async {
      final h = Harness(
        (r) => Reply(
          200,
          text: sse(
              'error', {'code': 'push_subscription', 'message': 'cannot pull'}),
          headers: {'content-type': 'text/event-stream'},
        ),
      );
      final s = h.fb.queues.subscribe('w', (d) {});
      await expectLater(
        s.done,
        throwsA(
          isA<RealtimeException>()
              .having((e) => e.code, 'code', 'push_subscription'),
        ),
      );
    });

    test('a server hiccup is retried with backoff', () async {
      var n = 0;
      final h = Harness(
        (r) {
          if (++n == 1) return apiError(500, 'boom', 'x');
          return Reply(
            200,
            stream: StreamController<List<int>>().stream,
            headers: {'content-type': 'text/event-stream'},
          );
        },
        retry: RetryOptions.none,
      );
      final errors = <Object>[];
      final s = h.fb.queues.subscribe('w', (d) {}, onError: errors.add);
      await waitFor(() => n == 2);
      expect(errors.single, isA<ServerException>());
      expect(h.sleeps, isNotEmpty);
      s.unsubscribe();
      await s.done;
    });
  });
}
