# forgebase (Dart and Flutter SDK)

Client for [ForgeBase](../../README.md): auth with automatic token refresh,
collections and records, realtime over WebSocket with reconnect and resume,
storage, key-value, functions, queues, jobs, entitlements and store billing.

- Pure Dart. It runs in Flutter on iOS, Android, web and desktop, and in
  server-side Dart. No `dart:io`, no `dart:html`, no Flutter dependency.
- Three dependencies, all BSD-3-Clause: [`http`](https://pub.dev/packages/http),
  [`web_socket_channel`](https://pub.dev/packages/web_socket_channel) and
  [`crypto`](https://pub.dev/packages/crypto) (SHA-256 for upload checksums).
- Dart 3.3 or newer. The code uses records and sealed classes.
- It mirrors the [JavaScript SDK](../js/README.md): same modules, same routes,
  same retry and error rules. Names follow Dart style (`ForgeBaseException`
  where JS has `ForgeBaseError`, `Duration` where JS has milliseconds).

Licence: MIT OR Apache-2.0.

## Install

The package is not on pub.dev yet. Use it from the repository:

```yaml
dependencies:
  forgebase:
    git:
      url: https://github.com/farzanshibu/forgebase
      path: sdk/dart
```

## Quick start

```dart
import 'package:forgebase/forgebase.dart';

final fb = ForgeBase('http://localhost:8090');

await fb.auth.signIn(email: 'ada@example.com', password: 'correct horse');

final posts = fb.collection('posts');
final created = await posts.create({'title': 'Hello'});
final page = await posts.list(const ListOptions(filter: 'title ~ "Hell%"', sort: '-created'));
print(page.items.length);
```

For servers and scripts use an API key (sent as `X-API-Key`) instead of a
session:

```dart
final fb = ForgeBase('https://api.example.com', apiKey: Platform.environment['FORGEBASE_API_KEY']);
```

Call `await fb.close()` when you are done: it closes the realtime connection
and the HTTP client the SDK created.

### Client options

```dart
ForgeBase(
  baseUrl,
  apiKey: ...,                 // X-API-Key (API key, service account key, PAT)
  accessToken: ...,            // a static bearer token, never refreshed
  storage: SecureTokenStorage(), // where the session lives, see below
  retry: const RetryOptions(maxRetries: 3),
  timeout: const Duration(seconds: 30), // time to response headers
  consistency: Consistency.session,
  headers: {'X-Forgebase-Org': 'acme'}, // sent with every request
  httpClient: myClient,        // package:http Client (tests, proxies)
  webSocketConnector: ...,     // see "Testing your app"
);
```

## Authentication

```dart
final res = await fb.auth.signIn(email: email, password: password);
switch (res) {
  case Session(:final user):
    print('hello ${user?['email']}');
  case MfaChallenge(:final mfaToken):
    final session = await fb.auth.verifyMfa(mfaToken, totpCode);
}
```

Also available: `signUp`, `signInAdmin`, `requestMagicLink` / `confirmMagicLink`,
`requestOtp` / `verifyOtp`, `requestPasswordReset` / `confirmPasswordReset`,
`requestEmailVerification` / `confirmEmailVerification`, `changePassword`,
`me`, `createToken`, `listSessions`, `revokeSession`, `signOut`.

**Refresh and rotation.** The SDK refreshes the access token 30 s before it
expires (`refreshSkew`) and retries a request once after a `401`. Concurrent
callers share one refresh request. Refresh tokens rotate: every refresh stores
the new one. A refresh token the server rejects (revoked, or reused) ends the
session and emits `signedOut`. A network failure during refresh keeps the
session. Set `autoRefresh: false` to turn all of this off.

**Auth events.**

```dart
final stop = fb.auth.onChange((c) => print('${c.event.name} ${c.session?.user}'));
// or: fb.auth.onAuthStateChange.listen(...)   // a broadcast Stream<AuthChange>
```

**OAuth on mobile.** `start` returns the provider URL and the `state`. Open the
URL with `flutter_web_auth_2` or `url_launcher`, then hand the redirect back:

```dart
final start = await fb.auth.oauth.start('github');
final redirect = await FlutterWebAuth2.authenticate(
  url: start.authorizationUrl,
  callbackUrlScheme: 'myapp',
);
await fb.auth.oauth.completeFromUrl('github', Uri.parse(redirect));
```

### Token storage

The session (access token, rotating refresh token, user) is kept in a
`TokenStorage`. The default `MemoryTokenStorage` forgets it when the process
ends. Implement three methods to keep it across launches:

```dart
abstract interface class TokenStorage {
  FutureOr<String?> read();
  FutureOr<void> write(String value);
  FutureOr<void> delete();
}
```

The SDK treats a storage failure as "signed out", never as a crash.

**Flutter: `flutter_secure_storage` (Keychain / Keystore).** The SDK has no
Flutter dependency, so the adapter is yours. Copy this:

```dart
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:forgebase/forgebase.dart';

class SecureTokenStorage implements TokenStorage {
  SecureTokenStorage([FlutterSecureStorage? storage])
      : _storage = storage ?? const FlutterSecureStorage();

  static const _key = 'forgebase.session';
  final FlutterSecureStorage _storage;

  @override
  Future<String?> read() => _storage.read(key: _key);

  @override
  Future<void> write(String value) => _storage.write(key: _key, value: value);

  @override
  Future<void> delete() => _storage.delete(key: _key);
}
```

`CallbackTokenStorage(read: ..., write: ..., delete: ...)` builds an adapter from
three closures. `shared_preferences` works the same way, but it is not
encrypted: prefer the secure store for refresh tokens.

## Records

```dart
final posts = fb.collection('posts');

await posts.list(const ListOptions(
  filter: 'status = "live" && views > {:min}',
  params: {'min': 10},          // typed values: sent as JSON to /records/query
  sort: '-created,title',
  expand: ['author'],
  fields: ['id', 'title'],
  perPage: 50,
));

await for (final post in posts.iterate(const ListOptions(perPage: 100))) { ... }
final everything = await posts.all();
final one = await posts.first('slug = "intro"');   // Row? (null when none)
final n = await posts.count('status = "live"');

final r = await posts.get('abc');
await posts.update('abc', {'title': 'New'});
await posts.replace('abc', {'title': 'New', 'views': 0});
await posts.delete('abc');
```

Rows are `Map<String, dynamic>`. To get your own type:

```dart
final posts = fb.collectionOf('posts', decode: Post.fromJson);   // RecordsApi<Post>
```

**Optimistic concurrency.** Every record has a `_version`. Pass it back as
`version:` and the SDK sends `If-Match`. A stale write throws
`ConflictException` (412):

```dart
try {
  await posts.update(r['id'] as String, {'title': 'x'}, version: r['_version'] as int);
} on ConflictException {
  // someone else wrote first: reload and merge
}
```

`pages()` yields `ListResult`s and follows `nextCursor`, else page numbers.
`export(TransferFormat.csv)` and `import(format, text)` move data in bulk.
`fb.collections` manages collection definitions (administrators).

## Realtime

One WebSocket carries every subscription. The SDK sends the access token as
`?access_token=` (browsers cannot set headers on a WebSocket), sends
heartbeats, reconnects with exponential backoff and jitter, and **resumes every
subscription from its last cursor**, so a reconnect misses nothing the server
still retains.

```dart
final sub = await fb.realtime.subscribe(
  const SubscribeOptions(collection: 'posts', filter: 'status = "live"'),
  (event) {
    switch (event) {
      case RecordEvent(:final type, :final data):
        print('${type.name}: ${data['title']}');
      case ResetEvent(:final reason):
        reload();            // the server could not replay: refetch, then continue
      case ChannelMessage() || PresenceEvent():
        break;
    }
  },
);
// later
await sub.unsubscribe();
```

As a stream (subscribes on listen, unsubscribes on cancel):

```dart
fb.realtime.watch(const SubscribeOptions(collection: 'posts')).listen(print);
```

Channels and presence:

```dart
await fb.realtime.subscribe(const SubscribeOptions(channel: 'room:1'), onMessage);
await fb.realtime.publish('room:1', {'text': 'hi'});          // socket, else REST
final members = await fb.realtime.joinPresence('room:1', meta: {'name': 'Ada'});
```

Connection state: `fb.realtime.state`, `stateChanges`, `onOpen`, `onClose`,
`onReconnecting`, `onError`. A refused subscription (permission, bad filter)
fails the `subscribe` future with the server's `RealtimeException` code. If the
server closes the socket with `4001` (credential expired) the SDK refreshes the
token before it reconnects. Change `ReconnectOptions(baseDelay, maxDelay,
maxAttempts)` through the client options.

Realtime events that carry a `commit` position feed the client's commit tokens
automatically (see [Consistency](#consistency)).

## Storage

```dart
await fb.storage.createBucket('avatars', visibility: 'private');

final bytes = await File('me.png').readAsBytes();
final info = await fb.storage.putObject(
  'avatars', 'users/42/me.png', bytes,
  contentType: 'image/png',
  checksumSha256: sha256Hex(bytes),     // the server rejects a mismatch
  metadata: {'owner': '42'},
);

final data = await fb.storage.getObject('avatars', 'users/42/me.png');
final tail = await fb.storage.getObjectRange('avatars', 'users/42/me.png', 1024);
final stream = await fb.storage.getObjectStream('avatars', 'users/42/me.png');
await File('copy.png').openWrite().addStream(stream.stream);

final page = await fb.storage.listObjects('avatars', prefix: 'users/', delimiter: '/');
await fb.storage.copyObject('avatars', 'a', 'b');
await fb.storage.deleteObject('avatars', 'b');
```

**Signed URLs** let a client without credentials (an `Image.network`, a browser
tab) fetch or upload one object until the URL expires:

```dart
final signed = await fb.storage.sign('avatars', 'users/42/me.png', expiresIn: const Duration(minutes: 10));
final url = fb.storage.signedUrlFor(signed);       // absolute https URL
final put = await fb.storage.sign('avatars', 'new.png', method: 'PUT', contentType: 'image/png');
await fb.storage.uploadSigned(put, bytes, contentType: 'image/png');
```

**Large files.** `uploadMultipart` splits the data into numbered parts
(default 8 MiB), uploads them in order with their checksums and aborts the
session when a part fails. `uploadResumable` uses sequential offsets and can
continue an interrupted upload from the server's `receivedBytes`:

```dart
await fb.storage.uploadMultipart('videos', 'clip.mp4', bytes, partSize: 5 * 1024 * 1024,
    onProgress: (sent, total) => print('$sent / $total'));

late String sessionId;
await fb.storage.uploadResumable('videos', 'clip.mp4', bytes,
    onSession: (s) => sessionId = s.id);            // persist it
// after a crash:
await fb.storage.uploadResumable('videos', 'clip.mp4', bytes, resumeId: sessionId);
```

The low-level calls (`createUpload`, `uploadPart`, `appendChunk`,
`completeUpload`, `abortUpload`, `getUpload`) are public.

## Key-value store

```dart
final kv = fb.kv('cache');                 // one namespace

await kv.set('session:1', 'token', ttl: const Duration(minutes: 30));
final v = await kv.getString('session:1'); // null when missing
await kv.set('blob', [0, 255, 7]);         // bytes travel as {"base64": ...}
await kv.getValue('blob');                 // Uint8List

await kv.incr('hits', by: 5);
await kv.hashSet('user:1', {'name': 'Ada'});
await kv.listPush('queue', ['a', 'b']);
await kv.zsetAdd('board', {'ada': 100, 'bob': 80});
await kv.exec(['ZRANGE', 'board', '0', '-1', 'WITHSCORES']);   // any Redis-style command
await for (final key in kv.keys(match: 'session:*')) { ... }

kv.subscribe('news').listen((m) => print(m.message));         // server-sent events
await kv.publish('news', 'hello');
```

## Functions

```dart
final out = await fb.functions.invoke('resize', payload: {'width': 200});   // decoded JSON
final res = await fb.functions.invokeRaw('report', path: '/pdf');           // status, headers, bytes
```

Management calls (`deploy`, `list`, `logs`, `versions`, `rollback`, `secrets`,
`putSecret`) need an administrator or a key with the right scope.

## Queues and jobs

```dart
await fb.queues.createTopic('orders');
await fb.queues.createSubscription('worker', 'orders', ackDeadlineMs: 30000);
await fb.queues.publish('orders', {'id': 1}, attributes: {'kind': 'rush'}, dedupId: 'order-1');

final stream = fb.queues.subscribe('worker', (d) async {
  await handle(d.data);     // returning acks, throwing nacks (redelivered after backoff)
});
stream.unsubscribe();
```

Streaming pull reconnects with backoff after a dropped stream and ends with a
failed `stream.done` when the server refuses it (permission, push subscription).
`pull`, `ack`, `nack`, `modifyDeadline`, `seek`, snapshots and dead letters are
also available.

```dart
final job = await fb.jobs.enqueue('report', payload: {'month': '2026-10'}, idempotencyKey: 'report-2026-10');
final sub = await fb.jobs.onProgress(job.id, (u) => print('${u.status.name} ${u.progress}'));
final done = await fb.jobs.wait(job.id, timeout: const Duration(minutes: 2));

await fb.jobs.createWorkflow(
  workflow('etl')
      .step('fetch', 'http.get', {'url': u})
      .step('load', 'db.load', null, const StepOptions(maxAttempts: 5)),
);
```

## Entitlements and billing

```dart
if (await fb.entitlements.has(feature: 'export')) { ... }
final decision = await fb.entitlements.checkQuota(const QuotaRequest(feature: 'api_calls', units: 10));
await fb.entitlements.consume(const QuotaRequest(feature: 'api_calls', idempotencyKey: 'req-1'));
await fb.entitlements.withReservation(
  const QuotaRequest(feature: 'ai_tokens', units: 2000, idempotencyKey: 'run-7'),
  (reservation) async => runModel(),     // committed on success, released on a throw
);
```

In-app purchases are verified server side:

```dart
await fb.billing.store.verify(
  StoreProvider.googlePlay,
  StoreReceipt(purchaseToken: token, productId: 'pro_monthly'),
);
await fb.billing.store.verify(StoreProvider.appStore, StoreReceipt(signedTransaction: jws));
```

## Consistency

With read replicas the server answers a write with `X-ForgeBase-Commit`. The SDK
keeps the highest position per set and sends it back as `X-ForgeBase-Read-After`,
so your next read sees your own write. Servers without replicas send nothing and
the SDK adds no header. `Consistency.strong` (client option or per call) reads
from the primary. `fb.commitTokens.clear()` forgets the positions; signing out
does it for you.

## Retries and errors

- `429` and `503` are retried for every method, `502` and `504` and network
  errors for idempotent methods (`GET`, `HEAD`, `OPTIONS`, `PUT`, `DELETE`).
  Backoff is exponential with full jitter; a `Retry-After` header (seconds or a
  date) is honoured when it is at most 60 s, else the error is thrown at once.
- A `429` whose code says a **quota** is exhausted is never retried: waiting does
  not help.
- A queue publish without dedup ids, a pull, a job without an idempotency key
  and a workflow creation are never retried: a repeat could do the work twice.
- Every failure is a `ForgeBaseException` with `status`, `code`, `message`,
  `requestId`, `details`, `retryAfter` and `body`.

| Dart | Status | JS equivalent |
|---|---|---|
| `ValidationException` (`fields`) | 400, 422 | `ValidationError` |
| `AuthenticationException` | 401 | `AuthenticationError` |
| `PermissionException` | 403 | `PermissionError` |
| `NotFoundException` | 404 | `NotFoundError` |
| `ConflictException` | 409, 412 | `ConflictError` |
| `RateLimitException` | 429 | `RateLimitError` |
| `QuotaExceededException` | 429 (quota) | `QuotaExceededError` |
| `ServerException` | 5xx | `ServerError` |
| `NetworkException` | 0 | `NetworkError` |
| `InvalidResponseException` | 2xx, unexpected body | `InvalidResponseError` |
| `RealtimeException` | 0 | `RealtimeError` |
| `JobFailedException` | 0 | `JobFailedError` |

```dart
try {
  await posts.create({'views': 1});
} on ValidationException catch (e) {
  print(e.fields['title']?.message);   // per-field errors from details.fields
} on ForgeBaseException catch (e) {
  print('${e.code} (${e.status}) request ${e.requestId}');
}
```

## Flutter

Create the client once and share it (`Provider`, `get_it`, a top-level `final`):

```dart
late final ForgeBase fb;

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  fb = ForgeBase('https://api.example.com', storage: SecureTokenStorage());
  await fb.auth.getSession();          // restores the stored session
  runApp(const MyApp());
}
```

A live list with `StreamBuilder`: load once, then apply realtime events.

```dart
class PostsView extends StatefulWidget {
  const PostsView({super.key});
  @override
  State<PostsView> createState() => _PostsViewState();
}

class _PostsViewState extends State<PostsView> {
  final _rows = <String, Row>{};
  Subscription? _sub;
  bool _loading = true;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    _sub = await fb.realtime.subscribe(
      const SubscribeOptions(collection: 'posts'),
      (e) {
        if (e is RecordEvent && mounted) {
          setState(() {
            if (e.type == RecordEventType.deleted) {
              _rows.remove(e.recordId);
            } else {
              _rows[e.recordId] = e.data;
            }
          });
        } else if (e is ResetEvent) {
          _fetch();
        }
      },
    );
    await _fetch();
  }

  Future<void> _fetch() async {
    final page = await fb.collection('posts').list(const ListOptions(sort: '-created'));
    if (!mounted) return;
    setState(() {
      _rows
        ..clear()
        ..addEntries(page.items.map((r) => MapEntry(r['id'] as String, r)));
      _loading = false;
    });
  }

  @override
  void dispose() {
    _sub?.unsubscribe();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => _loading
      ? const Center(child: CircularProgressIndicator())
      : ListView(children: [for (final r in _rows.values) ListTile(title: Text('${r['title']}'))]);
}
```

Images behind a private bucket: `Image.network(fb.storage.signedUrlFor(signed))`.

On Flutter web the SDK uses the browser's `fetch` and `WebSocket` through
`package:http` and `web_socket_channel`. The server must allow your origin
(CORS) and, for tokens, you use bearer sessions: cookie mode is not supported
(see below).

**App lifecycle.** The realtime socket survives short background periods and
reconnects when the OS drops it. To release it while the app is paused, call
`await fb.realtime.close()` in `didChangeAppLifecycleState(paused)` and
subscribe again on resume: `close()` forgets the subscriptions.

## Testing your app

Both transports are injectable. Pass an `http.Client` from `package:http/testing.dart`
and a WebSocket connector that returns your own `WebSocketChannel`:

```dart
final fb = ForgeBase(
  'http://fb.test',
  httpClient: MockClient((req) async => http.Response('{"status":"ok"}', 200)),
  webSocketConnector: (uri) => MyFakeChannel(uri),
  sleep: (_) async {},          // retries and reconnects do not wait
);
```

The SDK's own tests in [`test/`](test) do this: see `test/helpers.dart` for a
recording fake server and a scriptable fake WebSocket.

## Differences from the JavaScript SDK

- **No cookie mode and no CSRF handling.** Sessions are bearer tokens in a
  `TokenStorage`. Cookie sessions are a browser feature; Flutter web should use
  tokens too.
- **No SSE fallback for realtime.** Dart has WebSocket on every platform, so
  `realtime` is WebSocket only. Queue streaming pull and KV subscribe do use SSE.
- **No `AbortSignal`.** Cancel a stream by cancelling its subscription and
  bound a call with `Future.timeout`. Requests time out after `timeout` (30 s
  by default) while waiting for response headers.
- **Maps, not generated types.** Rows and most server objects are
  `Map<String, dynamic>` (`Row`, `Json`). Typed classes exist where the SDK has
  logic over the fields: `Session`, `ListResult`, `Bucket`, `ObjectInfo`,
  `ObjectPage`, `SignedUrl`, `Upload`, `PartInfo`, `KvKey`, `Job`, `JobUpdate`,
  `QueueMessage`, `ReceivedMessage`, `Published`, `AckResult` and the realtime
  events. They are hand-written against the OpenAPI document and covered by
  tests.
- **Records can be typed with `collectionOf`**, using a `fromJson` you write.
- **Durations** are `Duration`, not milliseconds, in public options.

## Development and tests

```sh
cd sdk/dart
dart pub get
dart analyze --fatal-infos
dart format --set-exit-if-changed .
dart test
```

`dart test` runs the unit tests (a `MockClient` for HTTP and a fake
`WebSocketChannel`) and skips `test/live_test.dart`. The live test drives the
SDK against a real server binary:

```sh
forgebase serve --port 8090 &      # in a fresh data directory, with an admin created
FORGEBASE_LIVE_URL=http://127.0.0.1:8090 \
FORGEBASE_LIVE_ADMIN_EMAIL=admin@example.test \
FORGEBASE_LIVE_ADMIN_PASSWORD=... \
dart test test/live_test.dart
```

`example/forgebase_example.dart` is a command-line tour (records, realtime,
storage, KV).

## Known limitations

- Version 0.1.0. The API may still change before 1.0.
- Request bodies are in memory: uploads take a `List<int>`. Use
  `uploadMultipart` or `uploadResumable` for large files, which send one part at
  a time but still hold the whole list.
- Downloads can stream (`getObjectStream`); the rest of the calls buffer the
  response.
- Realtime resume depends on the server's retention: a cursor that expired
  arrives as a `ResetEvent`, and you must refetch.
- Not wrapped yet: passkeys, backups, vector, graph, SQL and credits routes.
  Use `fb.request(path, method: ..., body: ...)` for any route.
