import 'package:forgebase/forgebase.dart';
import 'package:test/test.dart';

import 'helpers.dart';

void main() {
  group('entitlements', () {
    test('has asks for exactly one of feature, permission or plan', () async {
      final h = Harness((r) => ok({'has': true}));
      expect(await h.fb.entitlements.has(feature: 'export'), isTrue);
      expect(h.server.last.path, '/api/v1/entitlements/has');
      expect(h.server.last.query, {'feature': 'export'});
      await h.fb.entitlements.has(plan: 'pro', subject: 'org:1');
      expect(h.server.last.query, {'plan': 'pro', 'subject': 'org:1'});
      await expectLater(h.fb.entitlements.has(), throwsArgumentError);
      await expectLater(
        h.fb.entitlements.has(feature: 'a', plan: 'b'),
        throwsArgumentError,
      );
    });

    test('hasDetailed returns the reason; a no is false', () async {
      final h = Harness((r) => ok({'has': false, 'reason': 'plan_too_low'}));
      expect(await h.fb.entitlements.has(permission: 'p'), isFalse);
      expect((await h.fb.entitlements.hasDetailed(permission: 'p'))['reason'],
          'plan_too_low');
    });

    test('get and usage', () async {
      final h = Harness((r) => r.path == '/api/v1/usage'
          ? ok({
              'items': [
                {'feature': 'api'}
              ]
            })
          : ok({'plan': 'pro'}));
      expect((await h.fb.entitlements.get(subject: 'org:1'))['plan'], 'pro');
      expect(h.server.last.query, {'subject': 'org:1'});
      final u = await h.fb.entitlements.usage(scopeKey: 'k');
      expect(u.single['feature'], 'api');
      expect(h.server.last.query, {'scope_key': 'k'});
    });

    test('quota calls use the server field names', () async {
      final h = Harness((r) => ok({'id': 'res1', 'granted': true}));
      final e = h.fb.entitlements;
      await e.checkQuota(
          const QuotaRequest(feature: 'api', units: 3, scopeKey: 'u1'));
      expect(h.server.last.path, '/api/v1/quotas/check');
      expect(
          h.server.last.map, {'feature': 'api', 'units': 3, 'scope_key': 'u1'});
      await e.consume(const QuotaRequest(feature: 'api', idempotencyKey: 'k1'));
      expect(h.server.last.path, '/api/v1/usage/consume');
      expect(h.server.last.map['idempotency_key'], 'k1');
      await e.reserve(const QuotaRequest(
          feature: 'api', idempotencyKey: 'k2', ttlSecs: 60));
      expect(h.server.last.map['ttl_secs'], 60);
      await e.commitReservation('res 1', units: 2);
      expect(h.server.last.path, '/api/v1/usage/reservations/res%201/commit');
      expect(h.server.last.map, {'units': 2});
      await e.releaseReservation('res1');
      await e.acquireLease(
          const QuotaRequest(feature: 'jobs', idempotencyKey: 'k3'));
      await e.renewLease('l1', ttlSecs: 30);
      expect(h.server.last.map, {'ttl_secs': 30});
    });

    test('consume needs an idempotency key', () {
      final h = Harness((r) => ok({}));
      expect(
        () => h.fb.entitlements.consume(const QuotaRequest(feature: 'api')),
        throwsArgumentError,
      );
    });

    test('a hard limit surfaces as QuotaExceededException', () async {
      final h = Harness((r) => apiError(429, 'quota_exceeded', 'limit hit'));
      await expectLater(
        h.fb.entitlements
            .consume(const QuotaRequest(feature: 'api', idempotencyKey: 'k')),
        throwsA(isA<QuotaExceededException>()),
      );
      expect(h.server.log.length, 1, reason: 'waiting will not help');
    });

    test('withReservation commits on success and releases on failure',
        () async {
      final h = Harness((r) => ok({'id': 'res1'}));
      final out = await h.fb.entitlements.withReservation(
        const QuotaRequest(feature: 'api', idempotencyKey: 'k'),
        (res) async => 'done ${res['id']}',
      );
      expect(out, 'done res1');
      expect(h.server.last.path, '/api/v1/usage/reservations/res1/commit');

      final h2 = Harness((r) => ok({'id': 'res2'}));
      await expectLater(
        h2.fb.entitlements.withReservation(
          const QuotaRequest(feature: 'api', idempotencyKey: 'k'),
          (res) async => throw StateError('work failed'),
        ),
        throwsStateError,
      );
      expect(h2.server.last.path, '/api/v1/usage/reservations/res2/release');
    });
  });

  group('billing store', () {
    test('verify sends the store-specific fields', () async {
      final h = Harness((r) => ok({'status': 'active'}));
      await h.fb.billing.store.verify(
        StoreProvider.googlePlay,
        const StoreReceipt(
            purchaseToken: 'tok', productId: 'pro', kind: 'subscription'),
      );
      expect(h.server.last.path, '/api/v1/billing/store/google_play/verify');
      expect(h.server.last.map, {
        'purchase_token': 'tok',
        'product_id': 'pro',
        'kind': 'subscription',
      });
      await h.fb.billing.store.verify(
        StoreProvider.appStore,
        const StoreReceipt(signedTransaction: 'jws'),
      );
      expect(h.server.last.path, '/api/v1/billing/store/app_store/verify');
      expect(h.server.last.map, {'signed_transaction': 'jws'});
    });

    test('a purchase bound to another account is a conflict', () async {
      final h = Harness((r) => apiError(409, 'purchase_bound', 'taken'));
      await expectLater(
        h.fb.billing.store
            .verify(StoreProvider.creem, const StoreReceipt(checkoutId: 'c')),
        throwsA(isA<ConflictException>()),
      );
    });

    test('checkout and purchases', () async {
      final h = Harness((r) => r.path.endsWith('store-purchases')
          ? ok({
              'items': [
                {'id': 'p1'}
              ]
            })
          : ok({'url': 'https://pay'}));
      final page = await h.fb.billing.store.checkout(
        StoreProvider.creem,
        price: 'pro_monthly',
        successUrl: 'https://app/ok',
        idempotencyKey: 'k',
      );
      expect(page['url'], 'https://pay');
      expect(h.server.last.map, {
        'price': 'pro_monthly',
        'success_url': 'https://app/ok',
        'idempotency_key': 'k',
      });
      expect((await h.fb.billing.store.purchases(subject: 's')).single['id'],
          'p1');
      expect(h.server.last.query, {'subject': 's'});
    });
  });
}
