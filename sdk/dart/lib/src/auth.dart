import 'dart:async';
import 'dart:convert';

import 'errors.dart';
import 'http.dart';
import 'token_storage.dart';

/// What a sign-in call returns: a [Session] or an [MfaChallenge].
sealed class SignInResult {
  const SignInResult();
}

/// A signed-in session.
class Session extends SignInResult {
  /// Creates a session.
  const Session({
    required this.sessionId,
    required this.sessionExpiresAt,
    required this.expiresAt,
    this.accessToken,
    this.refreshToken,
    this.csrfToken,
    this.subject,
    this.user,
  });

  /// Reads a session stored by [toJson].
  factory Session.fromJson(Map<String, dynamic> j) => Session(
        accessToken: j['accessToken'] as String?,
        refreshToken: j['refreshToken'] as String?,
        expiresAt: (j['expiresAt'] as num).toInt(),
        sessionId: j['sessionId'] as String,
        sessionExpiresAt: j['sessionExpiresAt'] as String,
        csrfToken: j['csrfToken'] as String?,
        subject: j['subject'],
        user: j['user'] as Map<String, dynamic>?,
      );

  /// Reads the server's session JSON (`/api/auth/login`, `/refresh`, ...).
  factory Session.fromWire(Map<String, dynamic> w, DateTime now) {
    if (w['session_id'] is! String) {
      throw InvalidResponseException('sign-in response carries no session');
    }
    return Session(
      accessToken: w['access_token'] as String?,
      refreshToken: w['refresh_token'] as String?,
      expiresAt: now.millisecondsSinceEpoch +
          ((w['expires_in'] as num?) ?? 0).toInt() * 1000,
      sessionId: w['session_id'] as String,
      sessionExpiresAt: (w['session_expires_at'] as String?) ?? '',
      csrfToken: w['csrf_token'] as String?,
      subject: w['subject'],
      user: w['user'] as Map<String, dynamic>?,
    );
  }

  /// Bearer token.
  final String? accessToken;

  /// Rotating refresh token.
  final String? refreshToken;

  /// Access token expiry, epoch milliseconds.
  final int expiresAt;

  /// Server-side session id.
  final String sessionId;

  /// Absolute session expiry (RFC 3339).
  final String sessionExpiresAt;

  /// CSRF token (cookie sessions; unused by this SDK).
  final String? csrfToken;

  /// Who signed in (`{kind: "user", collection, id}` and friends).
  final Object? subject;

  /// The user record, for user sign-ins.
  final Map<String, dynamic>? user;

  /// A copy with a different [user].
  Session withUser(Map<String, dynamic>? u) => Session(
        accessToken: accessToken,
        refreshToken: refreshToken,
        expiresAt: expiresAt,
        sessionId: sessionId,
        sessionExpiresAt: sessionExpiresAt,
        csrfToken: csrfToken,
        subject: subject,
        user: u,
      );

  /// JSON form kept in a [TokenStorage].
  Map<String, dynamic> toJson() => {
        'accessToken': accessToken,
        'refreshToken': refreshToken,
        'expiresAt': expiresAt,
        'sessionId': sessionId,
        'sessionExpiresAt': sessionExpiresAt,
        'csrfToken': csrfToken,
        'subject': subject,
        'user': user,
      };
}

/// A second factor is required: call `auth.verifyMfa(mfaToken, code)`.
class MfaChallenge extends SignInResult {
  /// Creates a challenge.
  const MfaChallenge({required this.mfaToken, required this.expiresIn});

  /// Token to pass to `verifyMfa`.
  final String mfaToken;

  /// Seconds the challenge is valid.
  final int expiresIn;
}

/// Options shared by the sign-in flows.
class SignInOptions {
  /// Creates options.
  const SignInOptions({this.collection, this.deviceName});

  /// User collection (server default `users`).
  final String? collection;

  /// Name shown in the device list.
  final String? deviceName;
}

/// What happened to the session.
enum AuthEvent {
  /// A session was created or adopted.
  signedIn,

  /// The access token was rotated.
  tokenRefreshed,

  /// The session ended.
  signedOut,
}

/// One change of the auth state.
class AuthChange {
  /// Creates a change.
  const AuthChange(this.event, this.session);

  /// What happened.
  final AuthEvent event;

  /// The session after the change; `null` when signed out.
  final Session? session;
}

/// Auth settings of the client.
class AuthOptions {
  /// Creates auth options.
  const AuthOptions({
    this.apiKey,
    this.accessToken,
    this.storage,
    this.refreshSkew = const Duration(seconds: 30),
    this.autoRefresh = true,
    this.now,
  });

  /// Static API key / service-account key / PAT: sent as `X-API-Key`.
  final String? apiKey;

  /// Static bearer access token (no refresh).
  final String? accessToken;

  /// Where the session is kept; in memory by default.
  final TokenStorage? storage;

  /// Refresh the access token this long before it expires.
  final Duration refreshSkew;

  /// Refresh automatically (default true).
  final bool autoRefresh;

  /// Clock, for tests.
  final DateTime Function()? now;
}

/// Holds the session and implements the transport's credentials.
class AuthManager implements Credentials {
  /// Creates a manager.
  AuthManager(this._transport, [this.options = const AuthOptions()])
      : _storage = options.storage ?? MemoryTokenStorage(),
        _now = options.now ?? DateTime.now,
        apiKey = options.apiKey,
        _staticToken = options.accessToken;

  final Transport _transport;

  /// The settings.
  final AuthOptions options;
  final TokenStorage _storage;
  final DateTime Function() _now;
  final StreamController<AuthChange> _changes =
      StreamController<AuthChange>.broadcast(sync: true);
  Session? _session;
  bool _loaded = false;
  Future<Session>? _refreshing;

  /// API key sent as `X-API-Key`.
  String? apiKey;
  String? _staticToken;

  /// Uses a static bearer token for every request (`null` to stop).
  void setAccessToken(String? token) => _staticToken = token;

  /// Auth state changes.
  Stream<AuthChange> get changes => _changes.stream;

  Future<void> _load() async {
    if (_loaded) return;
    _loaded = true;
    try {
      final raw = await _storage.read();
      if (raw != null && raw.isNotEmpty && _session == null) {
        _session = Session.fromJson(jsonDecode(raw) as Map<String, dynamic>);
      }
    } on Object {
      // Unreadable storage: start signed out.
    }
  }

  /// The current session (after restoring it from storage).
  Future<Session?> getSession() async {
    await _load();
    return _session;
  }

  /// Current access token, refreshed first when it is about to expire.
  Future<String?> getAccessToken() async {
    if (_staticToken != null) return _staticToken;
    await _load();
    final s = _session;
    if (s?.accessToken == null) return null;
    if (options.autoRefresh &&
        s!.refreshToken != null &&
        s.expiresAt - options.refreshSkew.inMilliseconds <=
            _now().millisecondsSinceEpoch) {
      try {
        return (await refresh()).accessToken;
      } on Object {
        return s.accessToken;
      }
    }
    return s!.accessToken;
  }

  /// Adopts [s] (or clears the session with `null`) and stores it.
  Future<void> setSession(
    Session? s, [
    AuthEvent event = AuthEvent.signedIn,
  ]) async {
    _loaded = true;
    _session = s;
    try {
      if (s != null) {
        await _storage.write(jsonEncode(s.toJson()));
      } else {
        await _storage.delete();
      }
    } on Object {
      // Storage is best effort.
    }
    _changes.add(AuthChange(s == null ? AuthEvent.signedOut : event, s));
  }

  /// Rotates the refresh token. Concurrent callers share one request.
  Future<Session> refresh() {
    return _refreshing ??= _doRefresh().whenComplete(() => _refreshing = null);
  }

  Future<Session> _doRefresh() async {
    await _load();
    final rt = _session?.refreshToken;
    if (rt == null) {
      throw InvalidResponseException(
        'no refresh token: sign in first',
        status: 401,
      );
    }
    try {
      final w = await _transport.requestJson(
        '/api/auth/refresh',
        method: 'POST',
        body: {'refresh_token': rt},
        anonymous: true,
      );
      var next = Session.fromWire(w, _now());
      next = next.user == null ? next.withUser(_session?.user) : next;
      await setSession(next, AuthEvent.tokenRefreshed);
      return next;
    } on ForgeBaseException catch (e) {
      // A rejected refresh token (revoked, reused) ends the session;
      // transient failures keep it.
      if (e.status == 401) await setSession(null);
      rethrow;
    }
  }

  @override
  Future<Map<String, String>> headers(String method) async {
    final h = <String, String>{};
    if (apiKey != null) {
      h['X-API-Key'] = apiKey!;
      return h;
    }
    final token = await getAccessToken();
    if (token != null) h['Authorization'] = 'Bearer $token';
    return h;
  }

  @override
  Future<bool> onUnauthorized(Map<String, String> sent) async {
    if (apiKey != null ||
        !options.autoRefresh ||
        !sent.containsKey('Authorization')) {
      return false;
    }
    await _load();
    final s = _session;
    if (s?.refreshToken == null) return false;
    // Someone else already rotated the token since this request was built.
    if (s!.accessToken != null &&
        sent['Authorization'] != 'Bearer ${s.accessToken}') {
      return true;
    }
    try {
      await refresh();
      return true;
    } on Object {
      return false;
    }
  }

  /// Converts a sign-in style response and stores the session.
  Future<SignInResult> accept(Object? body) async {
    if (body is Map<String, dynamic> && body['mfa_required'] == true) {
      return MfaChallenge(
        mfaToken: '${body['mfa_token']}',
        expiresIn: (body['expires_in'] as num?)?.toInt() ?? 0,
      );
    }
    if (body is! Map<String, dynamic>) {
      throw InvalidResponseException('sign-in response carries no session');
    }
    final s = Session.fromWire(body, _now());
    await setSession(s);
    return s;
  }

  /// Stops the change stream.
  Future<void> dispose() => _changes.close();
}

Map<String, dynamic> _wire(SignInOptions o, Map<String, Object?> base) => {
      ...base,
      if (o.collection != null) 'collection': o.collection,
      if (o.deviceName != null) 'device_name': o.deviceName,
    };

/// `client.auth`: sign-up, sign-in flows, refresh and account helpers.
class AuthApi {
  /// Creates the API.
  AuthApi(this._t, this._mgr) : oauth = OAuthApi(_t, _mgr);

  final Transport _t;
  final AuthManager _mgr;

  /// Social / OIDC sign-in.
  final OAuthApi oauth;

  /// Sign-in, refresh and sign-out events.
  Stream<AuthChange> get onAuthStateChange => _mgr.changes;

  /// Subscribes to changes; returns a function that unsubscribes.
  void Function() onChange(void Function(AuthChange change) listener) {
    final sub = _mgr.changes.listen(listener);
    return () => unawaited(sub.cancel());
  }

  /// The current session, restored from storage when needed.
  Future<Session?> getSession() => _mgr.getSession();

  /// Current access token, refreshed first when it is about to expire.
  Future<String?> getAccessToken() => _mgr.getAccessToken();

  /// Adopts tokens obtained elsewhere (server hand-off, tests).
  Future<void> setSession(Session? s) => _mgr.setSession(s);

  /// Uses an API key (`fb_...`) for every request.
  void setApiKey(String? key) => _mgr.apiKey = key;

  /// Uses a static bearer token for every request (no refresh).
  void setAccessToken(String? token) => _mgr.setAccessToken(token);

  /// Creates a user. Does not sign in.
  Future<Map<String, dynamic>> signUp({
    required String email,
    required String password,
    String? collection,
    Map<String, dynamic>? data,
  }) async {
    final r = await _t.requestJson(
      '/api/auth/signup',
      method: 'POST',
      body: {
        'email': email,
        'password': password,
        if (collection != null) 'collection': collection,
        if (data != null) 'data': data,
      },
      anonymous: true,
    );
    return (r['user'] as Map<String, dynamic>?) ?? r;
  }

  /// Email + password. Returns a [Session], or an [MfaChallenge] when a second
  /// factor is needed.
  Future<SignInResult> signIn({
    required String email,
    required String password,
    SignInOptions options = const SignInOptions(),
  }) async =>
      _mgr.accept(
        await _post(
          '/api/auth/login',
          _wire(options, {'email': email, 'password': password}),
        ),
      );

  /// Administrator sign-in (`/api/auth/admins/login`).
  Future<SignInResult> signInAdmin({
    required String email,
    required String password,
    String? deviceName,
  }) async =>
      _mgr.accept(
        await _post(
          '/api/auth/admins/login',
          _wire(SignInOptions(deviceName: deviceName), {
            'email': email,
            'password': password,
          }),
        ),
      );

  /// Second step after `signIn` returned an [MfaChallenge]: a TOTP or recovery
  /// code.
  Future<Session> verifyMfa(String mfaToken, String code) async {
    final r = await _mgr.accept(
      await _post(
          '/api/auth/mfa/verify', {'mfa_token': mfaToken, 'code': code}),
    );
    if (r is Session) return r;
    throw InvalidResponseException(
        'MFA verification returned another challenge');
  }

  /// Emails a magic link; pass the `token` of its URL to [confirmMagicLink].
  Future<void> requestMagicLink(String email, {String? collection}) =>
      _post('/api/auth/magic-link/request', {
        'email': email,
        if (collection != null) 'collection': collection,
      });

  /// Signs in with a magic-link token.
  Future<SignInResult> confirmMagicLink(
    String token, {
    String? deviceName,
  }) async =>
      _mgr.accept(
        await _post(
          '/api/auth/magic-link/confirm',
          _wire(SignInOptions(deviceName: deviceName), {'token': token}),
        ),
      );

  /// Emails a one-time code.
  Future<void> requestOtp(String email, {String? collection}) =>
      _post('/api/auth/otp/request', {
        'email': email,
        if (collection != null) 'collection': collection,
      });

  /// Signs in with a one-time code.
  Future<SignInResult> verifyOtp({
    required String email,
    required String code,
    SignInOptions options = const SignInOptions(),
  }) async =>
      _mgr.accept(
        await _post(
          '/api/auth/otp/verify',
          _wire(options, {'email': email, 'code': code}),
        ),
      );

  /// Rotates the refresh token and returns the new session (also done
  /// automatically).
  Future<Session> refresh() => _mgr.refresh();

  /// Revokes the session on the server and forgets it locally.
  Future<void> signOut() async {
    try {
      if (await _mgr.getSession() != null) {
        await _t.request('/api/auth/logout', method: 'POST');
      }
    } finally {
      _t.commitTokens.clear();
      await _mgr.setSession(null);
    }
  }

  /// The caller as the server sees it (`subject`, `credential`, `mfa`, ...).
  Future<Map<String, dynamic>> me() => _t.requestJson('/api/auth/me');

  /// Emails a password-reset link.
  Future<void> requestPasswordReset(String email, {String? collection}) =>
      _post('/api/auth/password-reset/request', {
        'email': email,
        if (collection != null) 'collection': collection,
      });

  /// Sets a new password with the emailed token.
  Future<void> confirmPasswordReset(String token, String password) =>
      _post('/api/auth/password-reset/confirm', {
        'token': token,
        'password': password,
      });

  /// Emails a verification link.
  Future<void> requestEmailVerification(String email, {String? collection}) =>
      _post('/api/auth/verify-email/request', {
        'email': email,
        if (collection != null) 'collection': collection,
      });

  /// Verifies an email with the emailed token.
  Future<void> confirmEmailVerification(String token) =>
      _post('/api/auth/verify-email/confirm', {'token': token});

  /// Changes the password of the signed-in user.
  Future<void> changePassword(String current, String next) async {
    await _t.request(
      '/api/auth/password/change',
      method: 'POST',
      body: {'current_password': current, 'new_password': next},
    );
  }

  /// Creates a personal access token; the secret is only returned here.
  Future<Map<String, dynamic>> createToken({
    required String name,
    required List<String> scopes,
    int? expiresInDays,
  }) =>
      _t.requestJson(
        '/api/auth/tokens',
        method: 'POST',
        body: {
          'name': name,
          'scopes': scopes,
          if (expiresInDays != null) 'expires_in_days': expiresInDays,
        },
      );

  /// Sessions (devices) of the signed-in user.
  Future<Object?> listSessions() => _t.request('/api/auth/sessions');

  /// Revokes a session by id.
  Future<void> revokeSession(String id) async {
    await _t.request(
      '/api/auth/sessions/${Uri.encodeComponent(id)}',
      method: 'DELETE',
    );
  }

  Future<Object?> _post(String path, Map<String, Object?> body) =>
      _t.request(path, method: 'POST', body: body, anonymous: true);
}

/// Social / OIDC sign-in.
class OAuthApi {
  /// Creates the API.
  OAuthApi(this._t, this._mgr);

  final Transport _t;
  final AuthManager _mgr;

  /// Providers configured on the server.
  Future<List<dynamic>> providers() async {
    final r =
        await _t.requestJson('/api/auth/oauth/providers', anonymous: true);
    return (r['providers'] as List<dynamic>?) ?? const [];
  }

  /// URL to open in a browser: the server redirects to the provider.
  String redirectUrl(String provider,
          {SignInOptions options = const SignInOptions()}) =>
      _t.url('/api/auth/oauth/${Uri.encodeComponent(provider)}/start', {
        'collection': options.collection,
        'device_name': options.deviceName,
      });

  /// Returns the provider URL and the `state` instead of redirecting
  /// (native and server apps).
  Future<({String authorizationUrl, String state})> start(
    String provider, {
    SignInOptions options = const SignInOptions(),
  }) async {
    final r = await _t.requestJson(
      '/api/auth/oauth/${Uri.encodeComponent(provider)}/start',
      method: 'POST',
      body: _wire(options, {}),
      anonymous: true,
    );
    return (
      authorizationUrl: r['authorization_url'] as String,
      state: r['state'] as String,
    );
  }

  /// Finishes the flow with the `code` and `state` the provider sent to your
  /// redirect URI.
  Future<SignInResult> complete(
    String provider, {
    required String code,
    required String state,
  }) async =>
      _mgr.accept(
        await _t.request(
          '/api/auth/oauth/${Uri.encodeComponent(provider)}/callback',
          query: {'code': code, 'state': state},
          anonymous: true,
        ),
      );

  /// Parses `?code=...&state=...` (or `?error=...`) from the redirect URL and
  /// completes the flow.
  Future<SignInResult> completeFromUrl(String provider, Uri url) {
    final error = url.queryParameters['error'];
    final code = url.queryParameters['code'];
    final state = url.queryParameters['state'];
    if (error != null || code == null || state == null) {
      return Future.error(
        InvalidResponseException(
          error != null
              ? 'provider error: $error'
              : 'redirect URL has no code/state',
          status: 400,
        ),
      );
    }
    return complete(provider, code: code, state: state);
  }

  /// Starts linking a provider to the signed-in user.
  Future<({String authorizationUrl, String state})> link(
      String provider) async {
    final r = await _t.requestJson(
      '/api/auth/oauth/${Uri.encodeComponent(provider)}/link',
      method: 'POST',
    );
    return (
      authorizationUrl: r['authorization_url'] as String,
      state: r['state'] as String,
    );
  }

  /// Providers linked to the signed-in user.
  Future<Object?> identities() => _t.request('/api/auth/oauth/identities');

  /// Unlinks an identity.
  Future<void> unlink(String id) async {
    await _t.request(
      '/api/auth/oauth/identities/${Uri.encodeComponent(id)}',
      method: 'DELETE',
    );
  }
}
