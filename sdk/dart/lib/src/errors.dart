/// Typed errors. Every failure the SDK raises is a [ForgeBaseException].
library;

import 'dart:convert';

/// One field error of a validation failure (`details.fields`).
class FieldError {
  /// Creates a field error.
  const FieldError(this.code, this.message);

  /// Machine-readable code (`required`, `too_long`, ...).
  final String code;

  /// Human-readable message.
  final String message;

  @override
  String toString() => '$code: $message';
}

/// Base class of everything the SDK throws.
class ForgeBaseException implements Exception {
  /// Creates an exception.
  ForgeBaseException({
    required this.status,
    required this.code,
    required this.message,
    this.requestId,
    this.details,
    this.retryAfter,
    this.body,
  });

  /// HTTP status; `0` when no response was received.
  final int status;

  /// Stable machine-readable code (`validation_failed`, `network_error`, ...).
  final String code;

  /// Human-readable message.
  final String message;

  /// Id of the failed request (`x-request-id`).
  final String? requestId;

  /// Structured context from the error envelope.
  final Map<String, dynamic>? details;

  /// How long the server asked the client to wait (`Retry-After`).
  final Duration? retryAfter;

  /// Parsed response body, when there was one.
  final Object? body;

  @override
  String toString() => '$runtimeType($status $code): $message';
}

/// The server answered with an error status (anything but 2xx).
class ApiException extends ForgeBaseException {
  /// Creates an API exception.
  ApiException({
    required super.status,
    required super.code,
    required super.message,
    super.requestId,
    super.details,
    super.retryAfter,
    super.body,
  });
}

/// 400 / 422 `validation_failed` and other malformed requests.
class ValidationException extends ApiException {
  /// Creates a validation exception.
  ValidationException({
    required super.status,
    required super.code,
    required super.message,
    super.requestId,
    super.details,
    super.retryAfter,
    super.body,
  });

  /// Per-field errors from `details.fields`.
  Map<String, FieldError> get fields {
    final f = details?['fields'];
    final out = <String, FieldError>{};
    if (f is Map<String, dynamic>) {
      for (final e in f.entries) {
        final v = e.value;
        if (v is Map<String, dynamic>) {
          out[e.key] = FieldError(
            '${v['code'] ?? 'invalid'}',
            '${v['message'] ?? ''}',
          );
        }
      }
    }
    return out;
  }
}

/// 401: missing, expired or invalid credentials.
class AuthenticationException extends ApiException {
  /// Creates an authentication exception.
  AuthenticationException({
    required super.status,
    required super.code,
    required super.message,
    super.requestId,
    super.details,
    super.retryAfter,
    super.body,
  });
}

/// 403: the credentials are fine but the action is not allowed.
class PermissionException extends ApiException {
  /// Creates a permission exception.
  PermissionException({
    required super.status,
    required super.code,
    required super.message,
    super.requestId,
    super.details,
    super.retryAfter,
    super.body,
  });
}

/// 404.
class NotFoundException extends ApiException {
  /// Creates a not-found exception.
  NotFoundException({
    required super.status,
    required super.code,
    required super.message,
    super.requestId,
    super.details,
    super.retryAfter,
    super.body,
  });
}

/// 409 (unique violation, version mismatch, ...) and 412.
class ConflictException extends ApiException {
  /// Creates a conflict exception.
  ConflictException({
    required super.status,
    required super.code,
    required super.message,
    super.requestId,
    super.details,
    super.retryAfter,
    super.body,
  });
}

/// 429 from rate limiting. Already retried `maxRetries` times when thrown.
class RateLimitException extends ApiException {
  /// Creates a rate-limit exception.
  RateLimitException({
    required super.status,
    required super.code,
    required super.message,
    super.requestId,
    super.details,
    super.retryAfter,
    super.body,
  });
}

/// 429 because a quota is exhausted. Never retried: waiting will not help.
class QuotaExceededException extends ApiException {
  /// Creates a quota exception.
  QuotaExceededException({
    required super.status,
    required super.code,
    required super.message,
    super.requestId,
    super.details,
    super.retryAfter,
    super.body,
  });
}

/// 5xx. 503 is retried before this is thrown.
class ServerException extends ApiException {
  /// Creates a server exception.
  ServerException({
    required super.status,
    required super.code,
    required super.message,
    super.requestId,
    super.details,
    super.retryAfter,
    super.body,
  });
}

/// No HTTP response (DNS, refused connection, timeout).
class NetworkException extends ForgeBaseException {
  /// Creates a network exception.
  NetworkException(String message, {this.cause})
      : super(status: 0, code: 'network_error', message: message);

  /// The underlying error.
  final Object? cause;
}

/// The response was 2xx but not what the SDK expected.
class InvalidResponseException extends ForgeBaseException {
  /// Creates an invalid-response exception.
  InvalidResponseException(String message, {super.status = 200})
      : super(code: 'invalid_response', message: message);
}

/// WebSocket and event-stream failures.
class RealtimeException extends ForgeBaseException {
  /// Creates a realtime exception.
  RealtimeException(String code, String message)
      : super(status: 0, code: code, message: message);
}

final _quotaCodes =
    RegExp('quota|limit_exceeded|usage_limit', caseSensitive: false);

/// Builds the right exception from a failed response.
///
/// [body] is the decoded JSON body (or `null`), [headers] the response
/// headers with lower-case names.
ApiException errorFromResponse(
  int status,
  Object? body,
  Map<String, String> headers,
) {
  final env = body is Map<String, dynamic> ? body['error'] : null;
  final e = env is Map<String, dynamic> ? env : const <String, dynamic>{};
  final code = e['code'] is String ? e['code'] as String : 'http_error';
  final message =
      e['message'] is String ? e['message'] as String : 'HTTP $status';
  final retryAfter = parseRetryAfter(headers['retry-after']);
  final details = e['details'];
  String? requestId =
      e['request_id'] is String ? e['request_id'] as String : null;
  requestId ??= headers['x-request-id'];

  ApiException make(
    ApiException Function({
      required int status,
      required String code,
      required String message,
      String? requestId,
      Map<String, dynamic>? details,
      Duration? retryAfter,
      Object? body,
    }) ctor,
  ) =>
      ctor(
        status: status,
        code: code,
        message: message,
        requestId: requestId,
        details: details is Map<String, dynamic> ? details : null,
        retryAfter: retryAfter,
        body: body,
      );

  if (status == 429) {
    return _quotaCodes.hasMatch(code)
        ? make(QuotaExceededException.new)
        : make(RateLimitException.new);
  }
  if (status == 401) return make(AuthenticationException.new);
  if (status == 403) return make(PermissionException.new);
  if (status == 404) return make(NotFoundException.new);
  if (status == 409 || status == 412) return make(ConflictException.new);
  if (status == 400 || status == 422) return make(ValidationException.new);
  if (status >= 500) return make(ServerException.new);
  return make(ApiException.new);
}

/// Decodes an error body: JSON when it parses, else `null`.
Object? tryDecodeJson(String text) {
  if (text.isEmpty) return null;
  try {
    return jsonDecode(text);
  } on FormatException {
    return null;
  }
}

const _months = {
  'jan': 1, 'feb': 2, 'mar': 3, 'apr': 4, 'may': 5, 'jun': 6,
  'jul': 7, 'aug': 8, 'sep': 9, 'oct': 10, 'nov': 11, 'dec': 12, //
};

final _httpDate = RegExp(
  r'^(?:[A-Za-z]{3},\s+)?(\d{1,2})\s+([A-Za-z]{3})\s+(\d{4})\s+(\d{2}):(\d{2}):(\d{2})\s+GMT$',
);

/// `Retry-After` as a [Duration] (delta-seconds or HTTP-date); `null` when
/// absent or invalid.
Duration? parseRetryAfter(String? value, {DateTime? now}) {
  if (value == null) return null;
  final v = value.trim();
  final secs = double.tryParse(v);
  if (secs != null && RegExp(r'^\d+(\.\d+)?$').hasMatch(v)) {
    return Duration(milliseconds: (secs * 1000).round());
  }
  final m = _httpDate.firstMatch(v);
  if (m == null) return null;
  final month = _months[m.group(2)!.toLowerCase()];
  if (month == null) return null;
  final at = DateTime.utc(
    int.parse(m.group(3)!),
    month,
    int.parse(m.group(1)!),
    int.parse(m.group(4)!),
    int.parse(m.group(5)!),
    int.parse(m.group(6)!),
  );
  final diff = at.difference((now ?? DateTime.now()).toUtc());
  return diff.isNegative ? Duration.zero : diff;
}
