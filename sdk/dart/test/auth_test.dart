import 'dart:async';
import 'dart:convert';

import 'package:forgebase/forgebase.dart';
import 'package:test/test.dart';

import 'helpers.dart';

void main() {
  group('sign in', () {
    test('stores the session and sends the bearer token', () async {
      final h = Harness((r) {
        if (r.path == '/api/auth/login') return ok(wireSession());
        return ok({'ok': true});
      });
      final res = await h.fb.auth.signIn(
        email: 'a@b.c',
        password: 'pw',
        options: const SignInOptions(deviceName: 'phone'),
      );
      expect(res, isA<Session>());
      final s = res as Session;
      expect(s.accessToken, 'at1');
      expect(s.refreshToken, 'rt1');
      expect(s.sessionId, 'sess1');
      expect(s.user!['email'], 'a@b.c');
      expect(h.server.log.first.map, {
        'email': 'a@b.c',
        'password': 'pw',
        'device_name': 'phone',
      });
      // Credentials are not sent to the sign-in call itself.
      expect(h.server.log.first.headers.containsKey('Authorization'), isFalse);

      await h.fb.request('/api/auth/me');
      expect(h.server.last.headers['Authorization'], 'Bearer at1');
    });

    test('an MFA challenge is returned, not stored', () async {
      final h = Harness((r) {
        if (r.path == '/api/auth/login') {
          return ok(
              {'mfa_required': true, 'mfa_token': 'm1', 'expires_in': 300});
        }
        return ok(wireSession());
      });
      final res = await h.fb.auth.signIn(email: 'a@b.c', password: 'pw');
      expect(res, isA<MfaChallenge>());
      expect((res as MfaChallenge).mfaToken, 'm1');
      expect(res.expiresIn, 300);
      expect(await h.fb.auth.getSession(), isNull);

      final s = await h.fb.auth.verifyMfa('m1', '123456');
      expect(s.accessToken, 'at1');
      expect(h.server.last.map, {'mfa_token': 'm1', 'code': '123456'});
      expect(await h.fb.auth.getSession(), isNotNull);
    });

    test('a response without a session is rejected', () async {
      final h = Harness((r) => ok({'foo': 1}));
      await expectLater(
        h.fb.auth.signIn(email: 'a', password: 'b'),
        throwsA(isA<InvalidResponseException>()),
      );
    });

    test('sign up, magic link, otp and password flows hit their routes',
        () async {
      final h = Harness((r) {
        if (r.path == '/api/auth/signup') {
          return ok({
            'user': {'id': 'u9'},
          });
        }
        if (r.path.endsWith('/verify') || r.path.endsWith('/confirm')) {
          if (r.path.contains('otp') || r.path.contains('magic')) {
            return ok(wireSession());
          }
        }
        return Reply(204);
      });
      final user = await h.fb.auth.signUp(
        email: 'n@b.c',
        password: 'pw',
        data: {'name': 'N'},
      );
      expect(user['id'], 'u9');
      expect(h.server.last.map['data'], {'name': 'N'});

      await h.fb.auth.requestMagicLink('n@b.c');
      expect(h.server.last.path, '/api/auth/magic-link/request');
      expect(await h.fb.auth.confirmMagicLink('tok'), isA<Session>());
      await h.fb.auth.requestOtp('n@b.c');
      expect(
        await h.fb.auth.verifyOtp(email: 'n@b.c', code: '1'),
        isA<Session>(),
      );
      await h.fb.auth.requestPasswordReset('n@b.c');
      await h.fb.auth.confirmPasswordReset('t', 'new');
      expect(h.server.last.map, {'token': 't', 'password': 'new'});
      await h.fb.auth.requestEmailVerification('n@b.c');
      await h.fb.auth.confirmEmailVerification('t');
      await h.fb.auth.changePassword('old', 'new2');
      expect(h.server.last.map, {
        'current_password': 'old',
        'new_password': 'new2',
      });
    });
  });

  group('refresh and rotation', () {
    test('refreshes before the token expires and rotates the refresh token',
        () async {
      var clock = DateTime.utc(2026, 1, 1);
      var n = 0;
      final h = Harness(
        (r) {
          if (r.path == '/api/auth/login') {
            return ok(wireSession(expiresIn: 60));
          }
          if (r.path == '/api/auth/refresh') {
            n++;
            expect(r.map, {'refresh_token': 'rt1'});
            return ok(wireSession(access: 'at2', refresh: 'rt2'));
          }
          return ok({});
        },
        now: () => clock,
      );
      await h.fb.auth.signIn(email: 'a', password: 'b');
      clock = clock.add(const Duration(seconds: 20));
      await h.fb.request('/x');
      expect(n, 0, reason: 'still valid');
      expect(h.server.last.headers['Authorization'], 'Bearer at1');

      clock = clock.add(const Duration(seconds: 25)); // 5 s left < 30 s skew
      await h.fb.request('/x');
      expect(n, 1);
      expect(h.server.last.headers['Authorization'], 'Bearer at2');
      expect((await h.fb.auth.getSession())!.refreshToken, 'rt2');
    });

    test('concurrent refreshes share one request', () async {
      final gate = Completer<void>();
      var n = 0;
      final h = Harness((r) async {
        if (r.path == '/api/auth/login') return ok(wireSession());
        if (r.path == '/api/auth/refresh') {
          n++;
          await gate.future;
          return ok(wireSession(access: 'at2', refresh: 'rt2'));
        }
        return ok({});
      });
      await h.fb.auth.signIn(email: 'a', password: 'b');
      final a = h.fb.auth.refresh();
      final b = h.fb.auth.refresh();
      gate.complete();
      final r = await Future.wait([a, b]);
      expect(n, 1);
      expect(r[0].accessToken, 'at2');
      expect(r[1].accessToken, 'at2');
    });

    test('a 401 refreshes once and retries the request', () async {
      var stale = true;
      final h = Harness((r) {
        if (r.path == '/api/auth/login') return ok(wireSession());
        if (r.path == '/api/auth/refresh') {
          stale = false;
          return ok(wireSession(access: 'at2', refresh: 'rt2'));
        }
        if (r.headers['Authorization'] == 'Bearer at1' && stale) {
          return apiError(401, 'unauthorized', 'expired');
        }
        return ok({'fine': true});
      });
      await h.fb.auth.signIn(email: 'a', password: 'b');
      expect(await h.fb.request('/x'), {'fine': true});
      expect(h.server.where('POST', '/api/auth/refresh').length, 1);
      expect(h.server.last.headers['Authorization'], 'Bearer at2');
    });

    test('a second 401 after refreshing is surfaced', () async {
      final h = Harness((r) {
        if (r.path == '/api/auth/login') return ok(wireSession());
        if (r.path == '/api/auth/refresh') {
          return ok(wireSession(access: 'at2', refresh: 'rt2'));
        }
        return apiError(401, 'unauthorized', 'nope');
      });
      await h.fb.auth.signIn(email: 'a', password: 'b');
      await expectLater(
        h.fb.request('/x'),
        throwsA(isA<AuthenticationException>()),
      );
      expect(h.server.where('POST', '/api/auth/refresh').length, 1);
    });

    test('a rejected refresh token ends the session', () async {
      final events = <AuthEvent>[];
      final h = Harness((r) {
        if (r.path == '/api/auth/login') return ok(wireSession());
        if (r.path == '/api/auth/refresh') {
          return apiError(401, 'invalid_token', 'reused');
        }
        return apiError(401, 'unauthorized', 'expired');
      });
      h.fb.auth.onChange((c) => events.add(c.event));
      await h.fb.auth.signIn(email: 'a', password: 'b');
      await expectLater(
        h.fb.request('/x'),
        throwsA(isA<AuthenticationException>()),
      );
      expect(await h.fb.auth.getSession(), isNull);
      expect(events, [AuthEvent.signedIn, AuthEvent.signedOut]);
    });

    test('a transient refresh failure keeps the session', () async {
      var clock = DateTime.utc(2026, 1, 1);
      final h = Harness(
        (r) {
          if (r.path == '/api/auth/login') {
            return ok(wireSession(expiresIn: 10));
          }
          if (r.path == '/api/auth/refresh') {
            return apiError(500, 'boom', 'x');
          }
          return ok({});
        },
        now: () => clock,
        retry: RetryOptions.none,
      );
      await h.fb.auth.signIn(email: 'a', password: 'b');
      clock = clock.add(const Duration(minutes: 1));
      await h.fb.request('/x');
      expect(await h.fb.auth.getSession(), isNotNull);
      expect(h.server.last.headers['Authorization'], 'Bearer at1');
    });

    test('emits token_refreshed', () async {
      final events = <AuthEvent>[];
      final h = Harness((r) {
        if (r.path == '/api/auth/login') return ok(wireSession());
        return ok(wireSession(access: 'at2', refresh: 'rt2'));
      });
      h.fb.auth.onChange((c) => events.add(c.event));
      await h.fb.auth.signIn(email: 'a', password: 'b');
      await h.fb.auth.refresh();
      expect(events, [AuthEvent.signedIn, AuthEvent.tokenRefreshed]);
    });

    test('autoRefresh off never refreshes', () async {
      final server = FakeServer((r) {
        if (r.path == '/api/auth/login') return ok(wireSession());
        return apiError(401, 'unauthorized', 'x');
      });
      final fb = ForgeBase(base, httpClient: server.client, autoRefresh: false);
      await fb.auth.signIn(email: 'a', password: 'b');
      await expectLater(
        fb.request('/x'),
        throwsA(isA<AuthenticationException>()),
      );
      expect(server.where('POST', '/api/auth/refresh'), isEmpty);
    });
  });

  group('credentials', () {
    test('an API key is sent as X-API-Key and never refreshes', () async {
      final h =
          Harness((r) => apiError(401, 'unauthorized', 'x'), apiKey: 'fb_k');
      await expectLater(
        h.fb.request('/x'),
        throwsA(isA<AuthenticationException>()),
      );
      expect(h.server.last.headers['X-API-Key'], 'fb_k');
      expect(h.server.last.headers.containsKey('Authorization'), isFalse);
      expect(h.server.log.length, 1);
    });

    test('a static access token is sent as a bearer', () async {
      final h = Harness((r) => ok({}), accessToken: 'static');
      await h.fb.request('/x');
      expect(h.server.last.headers['Authorization'], 'Bearer static');
    });

    test('setApiKey switches credentials', () async {
      final h = Harness((r) => ok({}));
      await h.fb.request('/x');
      expect(h.server.last.headers.containsKey('X-API-Key'), isFalse);
      h.fb.auth.setApiKey('fb_new');
      await h.fb.request('/x');
      expect(h.server.last.headers['X-API-Key'], 'fb_new');
    });

    test('anonymous calls skip credentials', () async {
      final h = Harness((r) => ok({}), apiKey: 'fb_k');
      await h.fb.transport.request('/x', anonymous: true);
      expect(h.server.last.headers.containsKey('X-API-Key'), isFalse);
    });
  });

  group('token storage', () {
    test('persists the session and restores it in a new client', () async {
      final store = MemoryTokenStorage();
      final h = Harness((r) => ok(wireSession()), storage: store);
      await h.fb.auth.signIn(email: 'a', password: 'b');
      final raw = store.read()!;
      expect((jsonDecode(raw) as Map<String, dynamic>)['accessToken'], 'at1');

      final h2 = Harness((r) => ok({}), storage: store);
      final s = await h2.fb.auth.getSession();
      expect(s!.accessToken, 'at1');
      expect(s.user!['id'], 'u1');
      await h2.fb.request('/x');
      expect(h2.server.last.headers['Authorization'], 'Bearer at1');
    });

    test('signOut revokes, clears storage and commit tokens', () async {
      final store = MemoryTokenStorage();
      final h = Harness((r) {
        if (r.path == '/api/auth/login') return ok(wireSession());
        return Reply(204, headers: {'x-forgebase-commit': 's/1/1'});
      }, storage: store);
      await h.fb.auth.signIn(email: 'a', password: 'b');
      await h.fb.auth.signOut();
      expect(h.server.last.path, '/api/auth/logout');
      expect(store.read(), isNull);
      expect(h.fb.commitTokens.header(), isNull);
      expect(await h.fb.auth.getSession(), isNull);
    });

    test('signOut clears locally even when the server call fails', () async {
      final h = Harness((r) {
        if (r.path == '/api/auth/login') return ok(wireSession());
        return apiError(500, 'boom', 'x');
      }, retry: RetryOptions.none);
      await h.fb.auth.signIn(email: 'a', password: 'b');
      await expectLater(h.fb.auth.signOut(), throwsA(isA<ServerException>()));
      expect(await h.fb.auth.getSession(), isNull);
    });

    test('unreadable or throwing storage starts signed out', () async {
      final bad = CallbackTokenStorage(
        read: () => 'not json',
        write: (_) => throw StateError('disk full'),
        delete: () => throw StateError('locked'),
      );
      final h = Harness((r) => ok(wireSession()), storage: bad);
      expect(await h.fb.auth.getSession(), isNull);
      // Sign-in still works although the write fails.
      await h.fb.auth.signIn(email: 'a', password: 'b');
      expect(await h.fb.auth.getSession(), isNotNull);
    });

    test('supports async storage adapters', () async {
      String? slot;
      final store = CallbackTokenStorage(
        read: () async => slot,
        write: (v) async => slot = v,
        delete: () async => slot = null,
      );
      final h = Harness((r) => ok(wireSession()), storage: store);
      await h.fb.auth.signIn(email: 'a', password: 'b');
      expect(slot, isNotNull);
      await h.fb.auth.setSession(null);
      expect(slot, isNull);
    });
  });

  group('oauth', () {
    test('start and complete', () async {
      final h = Harness((r) {
        if (r.path.endsWith('/start')) {
          return ok({'authorization_url': 'https://idp/auth', 'state': 's1'});
        }
        return ok(wireSession());
      });
      final start = await h.fb.auth.oauth.start('github');
      expect(start.authorizationUrl, 'https://idp/auth');
      expect(start.state, 's1');
      final res = await h.fb.auth.oauth.completeFromUrl(
        'github',
        Uri.parse('myapp://cb?code=c1&state=s1'),
      );
      expect(res, isA<Session>());
      expect(h.server.last.query, {'code': 'c1', 'state': 's1'});
    });

    test('provider errors and missing params are rejected locally', () async {
      final h = Harness((r) => ok({}));
      await expectLater(
        h.fb.auth.oauth
            .completeFromUrl('x', Uri.parse('app://cb?error=denied')),
        throwsA(isA<InvalidResponseException>()),
      );
      await expectLater(
        h.fb.auth.oauth.completeFromUrl('x', Uri.parse('app://cb?code=1')),
        throwsA(isA<InvalidResponseException>()),
      );
      expect(h.server.log, isEmpty);
    });

    test('redirectUrl builds the browser URL', () {
      final h = Harness((r) => ok({}));
      expect(
        h.fb.auth.oauth.redirectUrl(
          'google',
          options: const SignInOptions(collection: 'staff'),
        ),
        '$base/api/auth/oauth/google/start?collection=staff',
      );
    });
  });
}
