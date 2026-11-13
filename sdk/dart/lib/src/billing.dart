import 'http.dart';

/// Stores a purchase can come from.
enum StoreProvider {
  /// Apple App Store.
  appStore('app_store'),

  /// Google Play.
  googlePlay('google_play'),

  /// Creem (web checkout).
  creem('creem');

  const StoreProvider(this.wire);

  /// Path segment on the wire.
  final String wire;
}

/// What the app received from the store; each store reads its own fields.
class StoreReceipt {
  /// Creates a receipt.
  const StoreReceipt({
    this.signedTransaction,
    this.transactionId,
    this.purchaseToken,
    this.productId,
    this.kind,
    this.checkoutId,
    this.subject,
  });

  /// App Store: the StoreKit 2 signed transaction (JWS).
  final String? signedTransaction;

  /// App Store: the transaction id.
  final String? transactionId;

  /// Google Play: the purchase token.
  final String? purchaseToken;

  /// Google Play: the product id.
  final String? productId;

  /// Google Play: `subscription` (default) or `one_time`.
  final String? kind;

  /// Creem: the checkout id of the success redirect.
  final String? checkoutId;

  /// Subject the purchase is for (default: the caller's).
  final String? subject;

  /// Request body in the server's shape.
  Map<String, Object?> toJson() => {
        if (signedTransaction != null) 'signed_transaction': signedTransaction,
        if (transactionId != null) 'transaction_id': transactionId,
        if (purchaseToken != null) 'purchase_token': purchaseToken,
        if (productId != null) 'product_id': productId,
        if (kind != null) 'kind': kind,
        if (checkoutId != null) 'checkout_id': checkoutId,
        if (subject != null) 'subject': subject,
      };
}

/// Merchant-of-record store helpers: `client.billing.store`.
class BillingStoreApi {
  /// Creates the API.
  BillingStoreApi(this._t);

  final Transport _t;

  /// Verifies a purchase server side with the store and applies it.
  /// Idempotent; a purchase bound to another account is a
  /// `ConflictException`.
  Future<Json> verify(StoreProvider provider, StoreReceipt receipt) =>
      _t.requestJson(
        '/api/v1/billing/store/${provider.wire}/verify',
        method: 'POST',
        body: receipt.toJson(),
      );

  /// A hosted checkout at a store that sells on the web (Creem): open the
  /// returned `url`, then [verify] with the `checkout_id`.
  Future<Json> checkout(
    StoreProvider provider, {
    required String price,
    required String successUrl,
    required String idempotencyKey,
    String? subject,
  }) =>
      _t.requestJson(
        '/api/v1/billing/store/${provider.wire}/checkout',
        method: 'POST',
        body: {
          'price': price,
          'success_url': successUrl,
          'idempotency_key': idempotencyKey,
          if (subject != null) 'subject': subject,
        },
      );

  /// Store purchases of [subject] (default: the caller).
  Future<List<Json>> purchases({String? subject}) async {
    final r = await _t.requestJson(
      '/api/v1/billing/store-purchases',
      query: {'subject': subject},
    );
    return [for (final i in (r['items'] as List<dynamic>)) i as Json];
  }
}

/// Billing helpers: `client.billing`.
class BillingApi {
  /// Creates the API.
  BillingApi(Transport t) : store = BillingStoreApi(t);

  /// Store purchases (App Store, Google Play, Creem).
  final BillingStoreApi store;
}
