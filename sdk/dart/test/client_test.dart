import 'dart:async';
import 'dart:convert';

import 'package:forgebase/forgebase.dart';
import 'package:http/http.dart' as http;
import 'package:test/test.dart';

import 'helpers.dart';

void main() {
  test('exposes every module', () async {
    final h = Harness((r) => ok({}));
    final fb = h.fb;
    expect(fb.auth, isA<AuthApi>());
    expect(fb.collections, isA<CollectionsApi>());
    expect(fb.collection('posts').name, 'posts');
    expect(fb.kv('ns').namespace, 'ns');
    expect(fb.entitlements, isA<EntitlementsApi>());
    expect(fb.billing.store, isA<BillingStoreApi>());
    expect(fb.queues, isA<QueuesApi>());
    expect(fb.jobs, isA<JobsApi>());
    expect(fb.storage, isA<StorageApi>());
    expect(fb.functions, isA<FunctionsApi>());
    expect(fb.realtime.state, RealtimeState.idle);
    expect(fb.baseUrl, base);
    fb.consistency = Consistency.strong;
    expect(fb.transport.consistency, Consistency.strong);
    await fb.close();
  });

  test('systemInfo and request', () async {
    final h = Harness((r) => ok({'version': '0.3.4'}));
    expect((await h.fb.systemInfo())['version'], '0.3.4');
    expect(h.server.last.path, '/api/system/info');
    await h.fb.request('/api/v1/kv/_info', method: 'GET');
    expect(h.server.last.path, '/api/v1/kv/_info');
  });

  test('close closes the client it created but not a borrowed one', () async {
    var closed = false;
    final borrowed = _Closing(() => closed = true);
    final fb = ForgeBase(base, httpClient: borrowed);
    await fb.close();
    expect(closed, isFalse);
    // A client the SDK created is closed without error.
    await ForgeBase(base).close();
  });

  test('works with the default token storage across calls', () async {
    final server = FakeServer((r) {
      if (r.path == '/api/auth/login') return ok(wireSession());
      return ok({'bearer': r.headers['Authorization']});
    });
    final fb = ForgeBase(base, httpClient: server.client);
    await fb.auth.signIn(email: 'a', password: 'b');
    final r = await fb.request('/x') as Map<String, dynamic>;
    expect(r['bearer'], 'Bearer at1');
    expect(jsonEncode(await fb.auth.getSession().then((s) => s!.toJson())),
        contains('at1'));
  });
}

class _Closing extends http.BaseClient {
  _Closing(this.onClose);
  final void Function() onClose;

  @override
  Future<http.StreamedResponse> send(http.BaseRequest request) =>
      Completer<http.StreamedResponse>().future;

  @override
  void close() => onClose();
}
