// Live tests against a running ForgeBase server. Skipped unless
// FORGEBASE_LIVE_URL is set:
//
//   FORGEBASE_LIVE_URL=http://127.0.0.1:8090 \
//   FORGEBASE_LIVE_ADMIN_EMAIL=admin@example.test \
//   FORGEBASE_LIVE_ADMIN_PASSWORD=... dart test test/live_test.dart
@TestOn('vm')
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:forgebase/forgebase.dart';
import 'package:test/test.dart';

import 'helpers.dart' show waitFor;

void main() {
  final env = Platform.environment;
  final url = env['FORGEBASE_LIVE_URL'];
  if (url == null) {
    test('live tests', () {}, skip: 'set FORGEBASE_LIVE_URL to run');
    return;
  }
  final suffix = DateTime.now().millisecondsSinceEpoch.toRadixString(36);

  late ForgeBase admin;

  setUpAll(() async {
    admin = ForgeBase(url);
    final res = await admin.auth.signInAdmin(
      email: env['FORGEBASE_LIVE_ADMIN_EMAIL']!,
      password: env['FORGEBASE_LIVE_ADMIN_PASSWORD']!,
    );
    expect(res, isA<Session>());
  });

  tearDownAll(() async => admin.close());

  test('health, system info and me', () async {
    expect(await admin.health(), isTrue);
    expect((await admin.systemInfo()), isNotEmpty);
    expect((await admin.auth.me()), isNotEmpty);
  });

  test('refresh rotates the session', () async {
    final before = (await admin.auth.getSession())!;
    final after = await admin.auth.refresh();
    expect(after.refreshToken, isNot(before.refreshToken));
    expect((await admin.auth.me()), isNotEmpty);
  });

  test('records: CRUD, filters, pagination, versions, realtime', () async {
    final name = 'posts_$suffix';
    await admin.collections.create({
      'name': name,
      'type': 'base',
      'fields': [
        {'name': 'title', 'type': 'text', 'required': true},
        {'name': 'views', 'type': 'number'},
      ],
    });
    addTearDown(() => admin.collections.delete(name));
    final posts = admin.collection(name);

    final events = <RealtimeEvent>[];
    final sub = await admin.realtime.subscribe(
      SubscribeOptions(collection: name),
      events.add,
    );

    final a = await posts.create({'title': 'one', 'views': 1});
    for (var i = 2; i <= 5; i++) {
      await posts.create({'title': 'p$i', 'views': i});
    }
    await waitFor(() => events.length >= 5);
    expect(events.first, isA<RecordEvent>());
    expect((events.first as RecordEvent).type, RecordEventType.created);
    expect(sub.cursor, isNotNull);

    expect(await posts.count(), 5);
    final filtered = await posts.list(
      const ListOptions(
        filter: 'views > {:min}',
        params: {'min': 3},
        sort: '-views',
      ),
    );
    expect(filtered.items.map((r) => r['views']), [5, 4]);

    final seen = <String>[];
    await for (final p
        in posts.pages(const ListOptions(perPage: 2, sort: 'views'))) {
      seen.addAll(p.items.map((r) => r['title']! as String));
    }
    expect(seen, ['one', 'p2', 'p3', 'p4', 'p5']);

    final id = a['id'] as String;
    final updated =
        await posts.update(id, {'views': 10}, version: a['_version'] as int?);
    expect(updated['views'], 10);
    await expectLater(
      posts.update(id, {'views': 11}, version: a['_version'] as int?),
      throwsA(isA<ConflictException>()),
    );
    await expectLater(
      posts.create({'views': 1}),
      throwsA(
        isA<ValidationException>()
            .having((e) => e.fields.containsKey('title'), 'title', isTrue),
      ),
    );
    await posts.delete(id);
    await expectLater(posts.get(id), throwsA(isA<NotFoundException>()));
    await sub.unsubscribe();
  });

  test(
      'storage: objects, versions of ranges, signed URLs, multipart, resumable',
      () async {
    final b = 'files-$suffix';
    final s = admin.storage;
    await s.createBucket(b);
    addTearDown(() => s.deleteBucket(b, force: true));

    final body = utf8.encode('hello world');
    final info = await s.putObject(
      b,
      'docs/a b.txt',
      body,
      contentType: 'text/plain',
      checksumSha256: sha256Hex(body),
      metadata: {'owner': 'dart'},
    );
    expect(info.size, 11);
    expect(info.metadata['owner'], 'dart');
    expect(await s.getObject(b, 'docs/a b.txt'), body);
    expect(utf8.decode(await s.getObjectRange(b, 'docs/a b.txt', 6)), 'world');
    expect((await s.metadata(b, 'docs/a b.txt')).contentType, 'text/plain');
    final page = await s.listObjects(b, delimiter: '/');
    expect(page.prefixes, ['docs/']);
    await expectLater(
      s.putObject(b, 'bad', [1], checksumSha256: '0' * 64),
      throwsA(isA<ValidationException>()),
    );

    final signed =
        await s.sign(b, 'docs/a b.txt', expiresIn: const Duration(minutes: 1));
    expect(await s.downloadSigned(signed), body);
    final up =
        await s.sign(b, 'signed.txt', method: 'PUT', contentType: 'text/plain');
    await s.uploadSigned(up, utf8.encode('via signed'),
        contentType: 'text/plain');
    expect(utf8.decode(await s.getObject(b, 'signed.txt')), 'via signed');

    await s.copyObject(b, 'signed.txt', 'copy.txt');
    await s.moveObject(b, 'copy.txt', 'moved.txt');
    expect(utf8.decode(await s.getObject(b, 'moved.txt')), 'via signed');

    final big = List<int>.generate(300000, (i) => i % 251);
    final multi =
        await s.uploadMultipart(b, 'big-multi.bin', big, partSize: 100000);
    expect(multi.size, big.length);
    expect(await s.getObject(b, 'big-multi.bin'), big);
    final res =
        await s.uploadResumable(b, 'big-res.bin', big, chunkSize: 128000);
    expect(res.size, big.length);
    expect(await s.getObject(b, 'big-res.bin'), big);

    await s.deleteObject(b, 'moved.txt');
    await expectLater(
        s.getObject(b, 'moved.txt'), throwsA(isA<NotFoundException>()));
  });

  test('kv: strings, counters, hashes, lists, scan, pubsub', () async {
    final kv = admin.kv('dart-$suffix');
    addTearDown(kv.flush);
    await kv.set('a', 'one', ttl: const Duration(minutes: 5));
    expect(await kv.getString('a'), 'one');
    expect((await kv.get('a')).ttl, isNotNull);
    expect(await kv.getValue('nope'), isNull);
    await kv.set('bin', [0, 255, 7]);
    expect(await kv.getValue('bin'), [0, 255, 7]);
    await kv.set('n', '5');
    await kv.incr('n', by: 3);
    expect(await kv.getString('n'), '8');
    await kv.hashSet('h', {'f': 'v', 'g': 'w'});
    expect((await kv.get('h')).value, {'f': 'v', 'g': 'w'});
    await kv.listPush('l', ['x', 'y']);
    expect((await kv.get('l')).value, ['x', 'y']);
    await kv.setAdd('s', ['m1', 'm2']);
    await kv.zsetAdd('z', {'top': 9, 'low': 1});
    expect(await kv.keys().toList(),
        containsAll(['a', 'bin', 'n', 'h', 'l', 's', 'z']));
    final r = await kv.exec(['INCRBY', 'n', 2]);
    expect(r, isNotEmpty);
    expect(await kv.getString('n'), '10');

    final got = <KvMessage>[];
    final sub = kv.subscribe('news').listen(got.add);
    await Future<void>.delayed(const Duration(milliseconds: 300));
    await kv.publish('news', 'hello');
    await waitFor(() => got.isNotEmpty);
    expect(got.single.message, 'hello');
    await sub.cancel();
  });

  test('queues and jobs', () async {
    final topic = 'dart_topic_$suffix';
    final q = admin.queues;
    await q.createTopic(topic);
    await q.createSubscription('dart_sub_$suffix', topic, start: 'earliest');
    addTearDown(() async {
      await q.deleteTopic(topic, force: true);
    });
    final p = await q.publish(topic, {'n': 1}, attributes: {'kind': 'a'});
    expect(p.seq, greaterThan(0));
    final msgs = await q.pull('dart_sub_$suffix', maxMessages: 5, waitMs: 1000);
    expect(msgs.single.message.data, {'n': 1});
    expect(msgs.single.message.attributes, {'kind': 'a'});
    final ack = await q.ack('dart_sub_$suffix', [msgs.single.ackId]);
    expect(ack.count, 1);

    final received = Completer<Object?>();
    final stream = q.subscribe('dart_sub_$suffix', (d) {
      if (!received.isCompleted) received.complete(d.data);
    });
    await Future<void>.delayed(const Duration(milliseconds: 300));
    await q.publish(topic, 'streamed');
    expect(
        await received.future.timeout(const Duration(seconds: 10)), 'streamed');
    stream.unsubscribe();
    await stream.done;

    final job = await admin.jobs
        .enqueue('dart.noop', payload: {'x': 1}, idempotencyKey: 'k-$suffix');
    expect(job.status, JobStatus.queued);
    final again = await admin.jobs
        .enqueue('dart.noop', payload: {'x': 1}, idempotencyKey: 'k-$suffix');
    expect(again.id, job.id);
    expect((await admin.jobs.get(job.id)).kind, 'dart.noop');
    await admin.jobs.cancel(job.id);
    final wf = await admin.jobs.createWorkflow(
      workflow('dart_wf_$suffix').step('a', 'dart.noop').step('b', 'dart.noop'),
    );
    expect(wf['id'], isNotNull);
    await admin.jobs.cancelWorkflow(wf['id'] as String);
  });

  test('entitlements and functions routes answer', () async {
    // An administrator has no billed subject of its own: name one.
    expect(
      await admin.entitlements.has(
        feature: 'nonexistent_$suffix',
        subject: 'org:none_$suffix',
      ),
      isFalse,
    );
    expect((await admin.functions.list()), isA<List<Json>>());
    await expectLater(
      admin.functions.invoke('does_not_exist_$suffix'),
      throwsA(isA<NotFoundException>()),
    );
  });

  test('anonymous clients are refused', () async {
    final anon = ForgeBase(url);
    addTearDown(anon.close);
    await expectLater(
      anon.collections
          .create({'name': 'nope', 'type': 'base', 'fields': <dynamic>[]}),
      throwsA(isA<AuthenticationException>()),
    );
  });
}
