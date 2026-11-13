// A command-line tour of the SDK against a running ForgeBase server.
//
//   FORGEBASE_URL=http://localhost:8090 \
//   FORGEBASE_API_KEY=fb_... \
//   dart run example/forgebase_example.dart [collection]
//
// Instead of an API key you can sign in as a user:
//   FORGEBASE_EMAIL=ada@example.com FORGEBASE_PASSWORD=... dart run ...
//
// The example creates one record in `collection` (default `posts`; it needs a
// `title` text field), watches the collection over the realtime WebSocket,
// stores a small file, and cleans up after itself.
import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:forgebase/forgebase.dart';

Future<void> main(List<String> args) async {
  final env = Platform.environment;
  final collection = args.isNotEmpty ? args.first : 'posts';
  final fb = ForgeBase(
    env['FORGEBASE_URL'] ?? 'http://localhost:8090',
    apiKey: env['FORGEBASE_API_KEY'],
  );

  try {
    if (!await fb.health()) {
      stderr.writeln('No ForgeBase server answers at ${fb.baseUrl}.');
      exitCode = 1;
      return;
    }
    print('server: ${(await fb.systemInfo())['version'] ?? 'unknown version'}');

    final email = env['FORGEBASE_EMAIL'];
    final password = env['FORGEBASE_PASSWORD'];
    if (env['FORGEBASE_API_KEY'] == null && email != null && password != null) {
      final res = await fb.auth.signIn(email: email, password: password);
      switch (res) {
        case Session(:final user):
          print('signed in as ${user?['email']}');
        case MfaChallenge():
          stderr.writeln('This account needs a second factor; not shown here.');
          exitCode = 1;
          return;
      }
    }

    await _records(fb, collection);
    await _storage(fb);
    await _kv(fb);
  } on ForgeBaseException catch (e) {
    // Every failure is a ForgeBaseException with a status and a stable code.
    stderr
        .writeln('${e.runtimeType}: ${e.code} (HTTP ${e.status}) ${e.message}');
    exitCode = 1;
  } finally {
    await fb.close();
  }
}

Future<void> _records(ForgeBase fb, String collection) async {
  final posts = fb.collection(collection);

  // Realtime: one WebSocket carries every subscription and resumes from the
  // last cursor after a reconnect.
  final seen = Completer<void>();
  final sub = await fb.realtime.subscribe(
    SubscribeOptions(collection: collection),
    (event) {
      if (event is RecordEvent) {
        print('realtime: ${event.type.name} ${event.recordId}');
        if (!seen.isCompleted) seen.complete();
      }
    },
  );

  final created = await posts.create({'title': 'Hello from Dart'});
  final id = created['id'] as String;
  print('created $id');
  await seen.future.timeout(const Duration(seconds: 5), onTimeout: () {});

  // Optimistic concurrency: a stale `version` throws ConflictException.
  final updated = await posts.update(
    id,
    {'title': 'Hello again'},
    version: created['_version'] as int?,
  );
  print('updated to "${updated['title']}"');

  final page = await posts.list(
    const ListOptions(sort: '-created', perPage: 5, skipTotal: true),
  );
  print('${page.items.length} most recent records');

  await posts.delete(id);
  await sub.unsubscribe();
}

Future<void> _storage(ForgeBase fb) async {
  const bucket = 'dart-example';
  await fb.storage.createBucket(bucket);
  try {
    final bytes = utf8.encode('hello from the Dart SDK');
    final info = await fb.storage.putObject(
      bucket,
      'greetings/hello.txt',
      bytes,
      contentType: 'text/plain',
      checksumSha256: sha256Hex(bytes),
    );
    print('stored ${info.key} (${info.size} bytes)');

    final signed = await fb.storage.sign(
      bucket,
      info.key,
      expiresIn: const Duration(minutes: 5),
    );
    print('share link: ${fb.storage.signedUrlFor(signed)}');
    print('download: ${utf8.decode(await fb.storage.downloadSigned(signed))}');
  } finally {
    await fb.storage.deleteBucket(bucket, force: true);
  }
}

Future<void> _kv(ForgeBase fb) async {
  final kv = fb.kv('dart-example');
  await kv.set('visits', '0', ttl: const Duration(minutes: 1));
  await kv.incr('visits', by: 3);
  print('visits = ${await kv.getString('visits')}');
  await kv.flush();
}
