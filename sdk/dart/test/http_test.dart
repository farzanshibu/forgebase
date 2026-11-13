import 'dart:async';

import 'package:forgebase/forgebase.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:test/test.dart';

import 'helpers.dart';

void main() {
  test('sends JSON bodies and decodes JSON answers', () async {
    final h = Harness((r) => ok({'echo': r.map}));
    final r = await h.fb.request('/api/x', method: 'POST', body: {'a': 1});
    expect(r, {
      'echo': {'a': 1},
    });
    expect(h.server.last.headers['Content-Type'], contains('application/json'));
    expect(h.server.last.headers['Accept'], 'application/json');
    expect(h.server.last.url.toString(), '$base/api/x');
  });

  test('builds query strings and skips nulls', () async {
    final h = Harness((r) => ok({}));
    await h.fb.request('/q', query: {'a': 'x y', 'b': null, 'c': true, 'd': 2});
    expect(h.server.last.url.query, 'a=x+y&c=true&d=2');
  });

  test('204 and empty bodies decode to null', () async {
    final h = Harness((r) => Reply(204));
    expect(await h.fb.request('/x', method: 'DELETE'), isNull);
  });

  test('invalid JSON in a 2xx answer is an InvalidResponseException', () async {
    final h = Harness((r) => Reply(200, text: 'nope'));
    expect(
      h.fb.request('/x'),
      throwsA(isA<InvalidResponseException>()),
    );
  });

  test('error envelope becomes a typed exception', () async {
    final h = Harness(
      (r) => apiError(404, 'not_found', 'no such record'),
    );
    try {
      await h.fb.request('/x');
      fail('should throw');
    } on NotFoundException catch (e) {
      expect(e.status, 404);
      expect(e.code, 'not_found');
      expect(e.message, 'no such record');
      expect(e.requestId, 'req_1');
    }
  });

  group('retries', () {
    test('429 honours Retry-After and succeeds', () async {
      var n = 0;
      final h = Harness(
        (r) => ++n < 3
            ? apiError(429, 'rate_limited', 'slow',
                headers: {'retry-after': '2'})
            : ok({'ok': true}),
      );
      expect(await h.fb.request('/x', method: 'POST', body: {}), {'ok': true});
      expect(n, 3);
      expect(
          h.sleeps, [const Duration(seconds: 2), const Duration(seconds: 2)]);
    });

    test('503 backs off exponentially with jitter', () async {
      var n = 0;
      final h = Harness(
        (r) => ++n < 4 ? apiError(503, 'unavailable', 'x') : ok({}),
      );
      await h.fb.request('/x');
      // random() = 0.5: half of 250, 500, 1000 ms.
      expect(h.sleeps.map((d) => d.inMilliseconds), [125, 250, 500]);
    });

    test('gives up after maxRetries with the last error', () async {
      final h = Harness(
        (r) => apiError(503, 'unavailable', 'x'),
        retry: const RetryOptions(maxRetries: 2),
      );
      await expectLater(h.fb.request('/x'), throwsA(isA<ServerException>()));
      expect(h.server.log.length, 3);
    });

    test('a Retry-After beyond the cap fails at once', () async {
      final h = Harness(
        (r) =>
            apiError(429, 'rate_limited', 'x', headers: {'retry-after': '600'}),
      );
      await expectLater(h.fb.request('/x'), throwsA(isA<RateLimitException>()));
      expect(h.server.log.length, 1);
    });

    test('quota 429 is a QuotaExceededException and is never retried',
        () async {
      final h = Harness((r) => apiError(429, 'quota_exceeded', 'no'));
      await expectLater(
        h.fb.request('/x'),
        throwsA(isA<QuotaExceededException>()),
      );
      expect(h.server.log.length, 1);
      expect(h.sleeps, isEmpty);
    });

    test('502 is retried for GET but not for POST', () async {
      var n = 0;
      final h = Harness((r) => ++n == 1 ? Reply(502) : ok({}));
      await h.fb.request('/x');
      expect(n, 2);

      final h2 = Harness((r) => Reply(502));
      await expectLater(
        h2.fb.request('/x', method: 'POST', body: {}),
        throwsA(isA<ServerException>()),
      );
      expect(h2.server.log.length, 1);
    });

    test('network errors retry idempotent methods only', () async {
      var n = 0;
      final h = Harness((r) {
        if (++n < 3) throw http.ClientException('reset');
        return ok({});
      });
      await h.fb.request('/x');
      expect(n, 3);

      final h2 = Harness((r) => throw http.ClientException('reset'));
      await expectLater(
        h2.fb.request('/x', method: 'POST', body: {}),
        throwsA(
          isA<NetworkException>()
              .having((e) => e.status, 'status', 0)
              .having((e) => e.code, 'code', 'network_error'),
        ),
      );
      expect(h2.server.log.length, 1);
    });
  });

  test('a slow server times out as NetworkException', () async {
    final t = Transport(
      baseUrl: base,
      client: MockClient((r) => Completer<http.Response>().future),
      timeout: const Duration(milliseconds: 20),
      retry: RetryOptions.none,
    );
    await expectLater(t.request('/x'), throwsA(isA<NetworkException>()));
  });

  group('consistency', () {
    test('echoes commit tokens back as Read-After', () async {
      final h = Harness((r) {
        if (r.method == 'POST') {
          return ok({}, headers: {'x-forgebase-commit': 'control/16/B374D848'});
        }
        return ok({});
      });
      await h.fb.request('/w', method: 'POST', body: {});
      await h.fb.request('/r');
      expect(h.server.last.headers['X-ForgeBase-Read-After'],
          'control/16/B374D848');
      expect(h.fb.commitTokens.get('control'), '16/B374D848');
    });

    test('a failed write still records its token', () async {
      final h = Harness(
        (r) => apiError(
          500,
          'boom',
          'x',
          headers: {'x-forgebase-commit': 's1/0/9'},
        ),
        retry: RetryOptions.none,
      );
      await expectLater(
        h.fb.request('/w', method: 'POST', body: {}),
        throwsA(isA<ServerException>()),
      );
      expect(h.fb.commitTokens.header(), 's1/0/9');
    });

    test('strong sends the consistency header', () async {
      final h = Harness((r) => ok({}), consistency: Consistency.strong);
      await h.fb.request('/r');
      expect(h.server.last.headers['X-ForgeBase-Consistency'], 'strong');
      final h2 = Harness((r) => ok({}));
      await h2.fb.request('/r');
      expect(h2.server.last.headers.containsKey('X-ForgeBase-Consistency'),
          isFalse);
    });
  });

  test('default headers are sent with every request', () async {
    final server = FakeServer((r) => ok({}));
    final fb = ForgeBase(
      base,
      httpClient: server.client,
      headers: {'X-Forgebase-Org': 'acme'},
    );
    await fb.request('/x');
    expect(server.last.headers['X-Forgebase-Org'], 'acme');
  });

  test('health is false on failure and never throws', () async {
    final h = Harness((r) => throw http.ClientException('down'));
    expect(await h.fb.health(), isFalse);
    final h2 = Harness((r) => Reply(200, text: 'ok'));
    expect(await h2.fb.health(), isTrue);
  });

  test('the base URL loses trailing slashes', () {
    final fb = ForgeBase('$base///');
    expect(fb.baseUrl, base);
    addTearDown(fb.close);
  });
}
