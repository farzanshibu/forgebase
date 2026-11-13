import 'dart:async';

import 'errors.dart';
import 'http.dart';
import 'sse.dart';

const _q = '/api/v1/queues';
String _enc(String s) => Uri.encodeComponent(s);

List<Json> _items(Json r) =>
    [for (final i in (r['items'] as List<dynamic>? ?? const [])) i as Json];

/// Result of one publish.
class Published {
  /// Creates a result.
  const Published({
    required this.messageId,
    required this.seq,
    required this.duplicate,
  });

  /// Reads the server's JSON.
  factory Published.fromJson(Json j) => Published(
        messageId: j['message_id'] as String,
        seq: (j['seq'] as num).toInt(),
        duplicate: j['duplicate'] == true,
      );

  /// Id of the stored (or, for a duplicate, the first) message.
  final String messageId;

  /// Sequence number in the topic.
  final int seq;

  /// The dedup id was seen inside the window: nothing was stored.
  final bool duplicate;
}

/// A message to publish.
class PublishMessage {
  /// Creates a message.
  const PublishMessage(
    this.data, {
    this.attributes,
    this.orderingKey,
    this.dedupId,
  });

  /// Payload (any JSON value).
  final Object? data;

  /// String attributes subscriptions can filter on.
  final Map<String, String>? attributes;

  /// Messages with one ordering key reach an ordered subscription in publish
  /// order.
  final String? orderingKey;

  /// A repeat inside the topic's dedup window is accepted once.
  final String? dedupId;

  /// Request body in the server's shape.
  Map<String, Object?> toJson() => {
        'data': data,
        if (attributes != null) 'attributes': attributes,
        if (orderingKey != null) 'ordering_key': orderingKey,
        if (dedupId != null) 'dedup_id': dedupId,
      };
}

/// A stored message.
class QueueMessage {
  /// Creates a message.
  const QueueMessage({
    required this.id,
    required this.topic,
    required this.seq,
    required this.data,
    required this.attributes,
    required this.raw,
  });

  /// Reads the server's JSON.
  factory QueueMessage.fromJson(Json j) => QueueMessage(
        id: j['id'] as String? ?? '',
        topic: j['topic'] as String? ?? '',
        seq: (j['seq'] as num?)?.toInt() ?? 0,
        data: j['data'],
        attributes: {
          for (final e in ((j['attributes'] as Map<String, dynamic>?) ??
                  const <String, dynamic>{})
              .entries)
            e.key: '${e.value}',
        },
        raw: j,
      );

  /// Message id.
  final String id;

  /// Topic name.
  final String topic;

  /// Sequence number in the topic.
  final int seq;

  /// Payload.
  final Object? data;

  /// String attributes.
  final Map<String, String> attributes;

  /// The whole JSON object (ordering key, publisher, timestamps, ...).
  final Json raw;
}

/// One delivery of a message, as pulled.
class ReceivedMessage {
  /// Creates a delivery.
  const ReceivedMessage({
    required this.ackId,
    required this.deliveryAttempt,
    required this.ackDeadline,
    required this.message,
  });

  /// Reads the server's JSON.
  factory ReceivedMessage.fromJson(Json j) => ReceivedMessage(
        ackId: j['ack_id'] as String,
        deliveryAttempt: (j['delivery_attempt'] as num?)?.toInt() ?? 1,
        ackDeadline: j['ack_deadline'] as String? ?? '',
        message: QueueMessage.fromJson(j['message'] as Json),
      );

  /// Id to ack / nack with.
  final String ackId;

  /// 1 on the first delivery.
  final int deliveryAttempt;

  /// RFC 3339 deadline to acknowledge by.
  final String ackDeadline;

  /// The message.
  final QueueMessage message;
}

/// Result of ack, nack and deadline changes.
class AckResult {
  /// Creates a result.
  const AckResult({required this.count, required this.unknown});

  /// Reads the server's JSON.
  factory AckResult.fromJson(Json j) => AckResult(
        count: (j['count'] as num?)?.toInt() ?? 0,
        unknown: [
          for (final u in (j['unknown'] as List<dynamic>? ?? const [])) '$u',
        ],
      );

  /// Ack ids settled.
  final int count;

  /// Ack ids the server no longer knows (expired and delivered again, or
  /// already settled).
  final List<String> unknown;
}

/// A delivery handed to a `subscribe` handler.
class Delivery {
  /// Creates a delivery.
  Delivery({
    required this.received,
    required Future<void> Function() ack,
    required Future<void> Function() nack,
    required Future<void> Function(Duration) extend,
  })  : _ack = ack,
        _nack = nack,
        _extend = extend;

  /// The raw delivery.
  final ReceivedMessage received;
  final Future<void> Function() _ack;
  final Future<void> Function() _nack;
  final Future<void> Function(Duration) _extend;

  /// The message payload.
  Object? get data => received.message.data;

  /// The message.
  QueueMessage get message => received.message;

  /// The message attributes.
  Map<String, String> get attributes => received.message.attributes;

  /// 1 on the first delivery.
  int get attempt => received.deliveryAttempt;

  /// Id to ack with.
  String get ackId => received.ackId;

  /// Acknowledges now (returning normally from the handler does it for you
  /// unless `autoAck` is off).
  Future<void> ack() => _ack();

  /// Negative acknowledgement: redelivered after the subscription's backoff.
  Future<void> nack() => _nack();

  /// Gives the handler more time (1 s to 10 min from now).
  Future<void> extend(Duration d) => _extend(d);
}

/// A running streaming pull.
class QueueStream {
  /// Creates a stream handle.
  QueueStream(this._stop, this.done);

  final void Function() _stop;

  /// Settles when the stream ends: completes after [unsubscribe], fails when
  /// the server refuses it for good.
  final Future<void> done;

  /// Stops the stream; deliveries still held return after their ack
  /// deadline.
  void unsubscribe() => _stop();
}

/// Topics, subscriptions, publish, pull and streaming pull: `client.queues`.
class QueuesApi {
  /// Creates the API.
  QueuesApi(this._t, {Sleeper? sleep}) : _sleep = sleep ?? defaultSleep;

  final Transport _t;
  final Sleeper _sleep;

  // ---- topics ----

  /// Every topic.
  Future<List<Json>> listTopics() async =>
      _items(await _t.requestJson('$_q/topics'));

  /// One topic.
  Future<Json> getTopic(String name) =>
      _t.requestJson('$_q/topics/${_enc(name)}');

  /// Creates a topic.
  Future<Json> createTopic(
    String name, {
    int? retentionMs,
    int? retentionBytes,
    int? maxMessageBytes,
    int? dedupWindowMs,
    Object? schema,
    Map<String, String>? labels,
  }) =>
      _t.requestJson(
        '$_q/topics',
        method: 'POST',
        body: {
          'name': name,
          if (retentionMs != null) 'retention_ms': retentionMs,
          if (retentionBytes != null) 'retention_bytes': retentionBytes,
          if (maxMessageBytes != null) 'max_message_bytes': maxMessageBytes,
          if (dedupWindowMs != null) 'dedup_window_ms': dedupWindowMs,
          if (schema != null) 'schema': schema,
          if (labels != null) 'labels': labels,
        },
      );

  /// Deletes a topic; [force] deletes its subscriptions too.
  Future<void> deleteTopic(String name, {bool force = false}) async {
    await _t.request(
      '$_q/topics/${_enc(name)}',
      method: 'DELETE',
      query: {'force': force ? true : null},
    );
  }

  /// Counters of a topic.
  Future<Json> topicMetrics(String name) =>
      _t.requestJson('$_q/topics/${_enc(name)}/metrics');

  /// Retained messages of a topic, oldest first (a read: nothing is
  /// consumed). `nextAfter` continues the listing.
  Future<({List<QueueMessage> items, int? nextAfter})> messages(
    String topic, {
    int? after,
    int? limit,
  }) async {
    final r = await _t.requestJson(
      '$_q/topics/${_enc(topic)}/messages',
      query: {'after': after, 'limit': limit},
    );
    return (
      items: [for (final m in _items(r)) QueueMessage.fromJson(m)],
      nextAfter: (r['next_after'] as num?)?.toInt(),
    );
  }

  // ---- publish ----

  /// Publishes one message.
  Future<Published> publish(
    String topic,
    Object? data, {
    Map<String, String>? attributes,
    String? orderingKey,
    String? dedupId,
  }) async {
    final r = await publishBatch(topic, [
      PublishMessage(
        data,
        attributes: attributes,
        orderingKey: orderingKey,
        dedupId: dedupId,
      ),
    ]);
    if (r.isEmpty) {
      throw InvalidResponseException('publish returned no result');
    }
    return r.first;
  }

  /// Publishes up to 1000 messages in one request; results are in the order
  /// sent.
  Future<List<Published>> publishBatch(
    String topic,
    List<PublishMessage> messages,
  ) async {
    // A dedup id makes a repeat harmless, so those publishes are safe to
    // retry after a network failure.
    final safe =
        messages.isNotEmpty && messages.every((m) => m.dedupId != null);
    final r = await _t.requestJson(
      '$_q/topics/${_enc(topic)}/publish',
      method: 'POST',
      body: {
        'messages': [for (final m in messages) m.toJson()]
      },
      retry: safe ? null : RetryOptions.none,
    );
    return [
      for (final i in (r['results'] as List<dynamic>? ?? const []))
        Published.fromJson(i as Json),
    ];
  }

  // ---- subscriptions ----

  /// Subscriptions, optionally of one topic.
  Future<List<Json>> listSubscriptions({String? topic}) async => _items(
      await _t.requestJson('$_q/subscriptions', query: {'topic': topic}));

  /// One subscription.
  Future<Json> getSubscription(String name) =>
      _t.requestJson('$_q/subscriptions/${_enc(name)}');

  /// Creates a subscription. [start] is `latest` (default), `earliest` or
  /// `{time: <RFC 3339>}`.
  Future<Json> createSubscription(
    String name,
    String topic, {
    Object? filter,
    int? ackDeadlineMs,
    int? maxDeliveryAttempts,
    int? backoffBaseMs,
    int? backoffMaxMs,
    String? deadLetterTopic,
    bool? ordering,
    Map<String, String>? push,
    Object? start,
    Map<String, String>? labels,
  }) =>
      _t.requestJson(
        '$_q/subscriptions',
        method: 'POST',
        body: {
          'name': name,
          'topic': topic,
          if (filter != null) 'filter': filter,
          if (ackDeadlineMs != null) 'ack_deadline_ms': ackDeadlineMs,
          if (maxDeliveryAttempts != null)
            'max_delivery_attempts': maxDeliveryAttempts,
          if (backoffBaseMs != null) 'backoff_base_ms': backoffBaseMs,
          if (backoffMaxMs != null) 'backoff_max_ms': backoffMaxMs,
          if (deadLetterTopic != null) 'dead_letter_topic': deadLetterTopic,
          if (ordering != null) 'ordering': ordering,
          if (push != null) 'push': push,
          if (start != null) 'start': start,
          if (labels != null) 'labels': labels,
        },
      );

  /// Deletes a subscription.
  Future<void> deleteSubscription(String name) async {
    await _t.request('$_q/subscriptions/${_enc(name)}', method: 'DELETE');
  }

  /// Counters of a subscription.
  Future<Json> subscriptionMetrics(String name) =>
      _t.requestJson('$_q/subscriptions/${_enc(name)}/metrics');

  /// Messages that used up their delivery attempts, newest first; [before] is
  /// a record id from the previous page.
  Future<List<Json>> deadLetters(
    String subscription, {
    int? before,
    int? limit,
  }) async =>
      _items(
        await _t.requestJson(
          '$_q/subscriptions/${_enc(subscription)}/dead-letters',
          query: {'before': before, 'limit': limit},
        ),
      );

  // ---- pull and ack ----

  /// Takes up to [maxMessages] ready messages (leasing them for the
  /// subscription's ack deadline). Acknowledge each with [ack].
  Future<List<ReceivedMessage>> pull(
    String subscription, {
    int? maxMessages,
    int? waitMs,
    String? consumer,
  }) async {
    // Pulling leases messages: a blind retry could lease them twice.
    final r = await _t.requestJson(
      '$_q/subscriptions/${_enc(subscription)}/pull',
      method: 'POST',
      body: {
        if (maxMessages != null) 'max_messages': maxMessages,
        if (waitMs != null) 'wait_ms': waitMs,
        if (consumer != null) 'consumer': consumer,
      },
      retry: RetryOptions.none,
    );
    return [
      for (final m in (r['messages'] as List<dynamic>? ?? const []))
        ReceivedMessage.fromJson(m as Json),
    ];
  }

  /// Acknowledges deliveries.
  Future<AckResult> ack(String subscription, List<String> ackIds) async =>
      AckResult.fromJson(
        await _t.requestJson(
          '$_q/subscriptions/${_enc(subscription)}/ack',
          method: 'POST',
          body: {'ack_ids': ackIds},
        ),
      );

  /// Negatively acknowledges deliveries: redelivered after the backoff.
  Future<AckResult> nack(String subscription, List<String> ackIds) async =>
      AckResult.fromJson(
        await _t.requestJson(
          '$_q/subscriptions/${_enc(subscription)}/nack',
          method: 'POST',
          body: {'ack_ids': ackIds},
        ),
      );

  /// Changes the ack deadline of held messages (1 s to 10 min from now;
  /// zero is a nack).
  Future<AckResult> modifyDeadline(
    String subscription,
    List<String> ackIds,
    Duration deadline,
  ) async =>
      AckResult.fromJson(
        await _t.requestJson(
          '$_q/subscriptions/${_enc(subscription)}/modify-deadline',
          method: 'POST',
          body: {'ack_ids': ackIds, 'ack_deadline_ms': deadline.inMilliseconds},
        ),
      );

  // ---- replay ----

  /// Moves the cursor back to [time] (inside the topic's retention) so later
  /// messages are delivered again, or restores a [snapshot]'s ack state.
  Future<Json> seek(String subscription, {DateTime? time, String? snapshot}) {
    if ((time == null) == (snapshot == null)) {
      throw ArgumentError('pass exactly one of time or snapshot');
    }
    return _t.requestJson(
      '$_q/subscriptions/${_enc(subscription)}/seek',
      method: 'POST',
      body: time != null
          ? {'time': time.toUtc().toIso8601String()}
          : {'snapshot': snapshot},
    );
  }

  /// Snapshots.
  Future<List<Json>> listSnapshots() async =>
      _items(await _t.requestJson('$_q/snapshots'));

  /// Saves the ack state of a subscription.
  Future<Json> createSnapshot(String name, String subscription) =>
      _t.requestJson(
        '$_q/snapshots',
        method: 'POST',
        body: {'name': name, 'subscription': subscription},
      );

  /// Deletes a snapshot.
  Future<void> deleteSnapshot(String name) async {
    await _t.request('$_q/snapshots/${_enc(name)}', method: 'DELETE');
  }

  // ---- streaming pull ----

  /// Streaming pull with acknowledgement: [handler] runs for every delivery as
  /// the server pushes it (server-sent events, flow-controlled by
  /// [maxMessages] / [maxBytes]). When the handler completes the message is
  /// acked; when it throws it is nacked and redelivered after the
  /// subscription's backoff ([autoAck] `false` leaves both to the handler).
  /// A dropped stream reconnects with backoff; a refusal (missing permission,
  /// push subscription, no such subscription) ends it and fails `done`.
  QueueStream subscribe(
    String subscription,
    FutureOr<void> Function(Delivery d) handler, {
    int? maxMessages,
    int? maxBytes,
    bool autoAck = true,
    bool reconnect = true,
    void Function(Object error)? onError,
  }) {
    var stopped = false;
    final cancel = Completer<void>();
    void report(Object e) {
      try {
        onError?.call(e);
      } on Object {
        // A throwing error callback must not break the stream.
      }
    }

    Future<void> settle(bool ack, String id) async {
      try {
        if (ack) {
          await this.ack(subscription, [id]);
        } else {
          await nack(subscription, [id]);
        }
      } on Object catch (e) {
        report(e);
      }
    }

    Future<void> deliver(ReceivedMessage r) async {
      var settled = false;
      Future<void> Function() once(bool ack) => () async {
            if (settled) return;
            settled = true;
            if (ack) {
              await this.ack(subscription, [r.ackId]);
            } else {
              await nack(subscription, [r.ackId]);
            }
          };
      final d = Delivery(
        received: r,
        ack: once(true),
        nack: once(false),
        extend: (dur) async {
          await modifyDeadline(subscription, [r.ackId], dur);
        },
      );
      try {
        await handler(d);
        if (autoAck && !settled) {
          settled = true;
          await settle(true, r.ackId);
        }
      } on Object catch (e) {
        report(e);
        if (autoAck && !settled) {
          settled = true;
          await settle(false, r.ackId);
        }
      }
    }

    Future<void> run() async {
      var delay = const Duration(milliseconds: 500);
      while (!stopped) {
        var opened = false;
        try {
          final res = await _t.send(
            '$_q/subscriptions/${_enc(subscription)}/events',
            headers: {'Accept': 'text/event-stream'},
            query: {'max_messages': maxMessages, 'max_bytes': maxBytes},
            retry: RetryOptions.none,
          );
          opened = true;
          final inflight = <Future<void>>{};
          final events = parseSse(res.stream);
          final sub = events.listen(null);
          final ended = Completer<void>();
          sub.onData((ev) {
            if (stopped) return;
            if (ev.event == 'error') {
              var code = 'stream_error';
              var message = ev.data;
              final j = tryDecodeJson(ev.data);
              if (j is Map<String, dynamic>) {
                code = j['code'] as String? ?? code;
                message = j['message'] as String? ?? message;
              }
              if (!ended.isCompleted) {
                ended.completeError(RealtimeException(code, message));
              }
              return;
            }
            if (ev.event != 'message' || ev.data.isEmpty) return;
            final j = tryDecodeJson(ev.data);
            if (j is! Map<String, dynamic>) return;
            final ReceivedMessage r;
            try {
              r = ReceivedMessage.fromJson(j);
            } on Object {
              return;
            }
            late final Future<void> p;
            p = deliver(r).whenComplete(() => inflight.remove(p));
            inflight.add(p);
          });
          sub.onError((Object e) {
            if (!ended.isCompleted) ended.completeError(e);
          });
          sub.onDone(() {
            if (!ended.isCompleted) ended.complete();
          });
          unawaited(cancel.future.then((_) => sub.cancel()));
          try {
            await Future.any<void>([ended.future, cancel.future]);
          } finally {
            await sub.cancel();
          }
          await Future.wait(inflight.toList());
          if (stopped || !reconnect) return;
        } on Object catch (e) {
          if (stopped) return;
          // A refusal is final; a network failure or a server hiccup is worth
          // another try.
          final isFinal = e is RealtimeException ||
              (e is ApiException &&
                  e.status >= 400 &&
                  e.status < 500 &&
                  e.status != 429 &&
                  e.status != 408);
          if (isFinal || !reconnect) rethrow;
          report(e);
        }
        if (opened) delay = const Duration(milliseconds: 500);
        await Future.any<void>([_sleep(delay), cancel.future]);
        final next = delay * 2;
        delay = next > const Duration(seconds: 10)
            ? const Duration(seconds: 10)
            : next;
      }
    }

    final done = run();
    // `done` may fail with nobody listening yet; the caller still gets the
    // failure when they await it.
    done.ignore();
    return QueueStream(() {
      stopped = true;
      if (!cancel.isCompleted) cancel.complete();
    }, done);
  }
}
