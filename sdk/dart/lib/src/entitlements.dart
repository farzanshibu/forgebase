import 'http.dart';

/// A quota request.
class QuotaRequest {
  /// Creates a request.
  const QuotaRequest({
    required this.feature,
    this.units,
    this.scopeKey,
    this.subject,
    this.metadata,
    this.idempotencyKey,
    this.ttlSecs,
  });

  /// Metered feature.
  final String feature;

  /// Units (default 1).
  final int? units;

  /// Counter for per-user / team / resource quotas.
  final String? scopeKey;

  /// Billed subject (default: the caller's).
  final String? subject;

  /// Free-form metadata stored with the usage.
  final Map<String, Object?>? metadata;

  /// Required for consume / reserve / lease: a replay returns the first
  /// result.
  final String? idempotencyKey;

  /// Hold time of reservations and leases.
  final int? ttlSecs;

  /// Request body in the server's shape.
  Map<String, Object?> toJson() => {
        'feature': feature,
        if (units != null) 'units': units,
        if (scopeKey != null) 'scope_key': scopeKey,
        if (subject != null) 'subject': subject,
        if (metadata != null) 'metadata': metadata,
        if (idempotencyKey != null) 'idempotency_key': idempotencyKey,
        if (ttlSecs != null) 'ttl_secs': ttlSecs,
      };
}

String _enc(String s) => Uri.encodeComponent(s);

/// Entitlement and quota helpers: `client.entitlements`.
class EntitlementsApi {
  /// Creates the API.
  EntitlementsApi(this._t);

  final Transport _t;

  /// `has(feature: ...)`, `has(permission: ...)` or `has(plan: ...)`: the
  /// boolean answer.
  Future<bool> has({
    String? feature,
    String? permission,
    String? plan,
    String? subject,
  }) async =>
      (await hasDetailed(
        feature: feature,
        permission: permission,
        plan: plan,
        subject: subject,
      ))['has'] ==
      true;

  /// Same as [has] with the `reason` when the answer is no.
  Future<Json> hasDetailed({
    String? feature,
    String? permission,
    String? plan,
    String? subject,
  }) {
    final given = [feature, permission, plan].where((e) => e != null).length;
    if (given != 1) {
      throw ArgumentError('pass exactly one of feature, permission or plan');
    }
    return _t.requestJson(
      '/api/v1/entitlements/has',
      query: {
        'feature': feature,
        'permission': permission,
        'plan': plan,
        'subject': subject,
      },
    );
  }

  /// Effective plan and grants of the caller (or of [subject], e.g.
  /// `org:<id>`).
  Future<Json> get({String? subject}) =>
      _t.requestJson('/api/v1/entitlements', query: {'subject': subject});

  /// Usage of every metered feature.
  Future<List<Json>> usage({String? subject, String? scopeKey}) async {
    final r = await _t.requestJson(
      '/api/v1/usage',
      query: {'subject': subject, 'scope_key': scopeKey},
    );
    return [for (final i in (r['items'] as List<dynamic>)) i as Json];
  }

  /// Would these units be granted right now? No side effects.
  Future<Json> checkQuota(QuotaRequest r) => _t.requestJson(
        '/api/v1/quotas/check',
        method: 'POST',
        body: r.toJson(),
      );

  /// Records usage at once. A replayed key returns the first result with
  /// `replayed: true`. Throws `QuotaExceededException` past a hard limit.
  Future<Json> consume(QuotaRequest r) {
    _needKey(r);
    return _t.requestJson(
      '/api/v1/usage/consume',
      method: 'POST',
      body: r.toJson(),
    );
  }

  /// Holds units until [commitReservation] or [releaseReservation] (or
  /// `ttlSecs`).
  Future<Json> reserve(QuotaRequest r) {
    _needKey(r);
    return _t.requestJson(
      '/api/v1/usage/reservations',
      method: 'POST',
      body: r.toJson(),
    );
  }

  /// Commits a reservation (optionally with the units actually used).
  Future<Json> commitReservation(String id, {int? units}) => _t.requestJson(
        '/api/v1/usage/reservations/${_enc(id)}/commit',
        method: 'POST',
        body: {if (units != null) 'units': units},
      );

  /// Releases a reservation.
  Future<Json> releaseReservation(String id) => _t.requestJson(
        '/api/v1/usage/reservations/${_enc(id)}/release',
        method: 'POST',
        body: <String, Object?>{},
      );

  /// Reserves, runs [fn], then commits; releases the hold when [fn] throws.
  Future<R> withReservation<R>(
    QuotaRequest r,
    Future<R> Function(Json reservation) fn,
  ) async {
    final res = await reserve(r);
    final id = res['id'] as String;
    try {
      final out = await fn(res);
      await commitReservation(id);
      return out;
    } on Object {
      try {
        await releaseReservation(id);
      } on Object {
        // The original failure matters more.
      }
      rethrow;
    }
  }

  /// Concurrency lease (e.g. "at most 5 running jobs").
  Future<Json> acquireLease(QuotaRequest r) {
    _needKey(r);
    return _t.requestJson(
      '/api/v1/usage/leases',
      method: 'POST',
      body: r.toJson(),
    );
  }

  /// Extends a lease.
  Future<Json> renewLease(String id, {int? ttlSecs}) => _t.requestJson(
        '/api/v1/usage/leases/${_enc(id)}/renew',
        method: 'POST',
        body: {if (ttlSecs != null) 'ttl_secs': ttlSecs},
      );

  /// Releases a lease.
  Future<void> releaseLease(String id) async {
    await _t.request('/api/v1/usage/leases/${_enc(id)}', method: 'DELETE');
  }

  void _needKey(QuotaRequest r) {
    if (r.idempotencyKey == null) {
      throw ArgumentError.value(
        null,
        'idempotencyKey',
        'required for consume, reserve and lease',
      );
    }
  }
}
