import 'package:forgebase/forgebase.dart';
import 'package:test/test.dart';

void main() {
  Map<String, dynamic> env(String code, [Map<String, dynamic>? details]) => {
        'error': {
          'code': code,
          'message': 'm',
          'request_id': 'r1',
          if (details != null) 'details': details,
        },
      };

  test('maps statuses to typed exceptions', () {
    expect(errorFromResponse(400, env('x'), {}), isA<ValidationException>());
    expect(errorFromResponse(422, env('x'), {}), isA<ValidationException>());
    expect(
        errorFromResponse(401, env('x'), {}), isA<AuthenticationException>());
    expect(errorFromResponse(403, env('x'), {}), isA<PermissionException>());
    expect(errorFromResponse(404, env('x'), {}), isA<NotFoundException>());
    expect(errorFromResponse(409, env('x'), {}), isA<ConflictException>());
    expect(errorFromResponse(412, env('x'), {}), isA<ConflictException>());
    expect(errorFromResponse(429, env('rate_limited'), {}),
        isA<RateLimitException>());
    expect(errorFromResponse(429, env('quota_exceeded'), {}),
        isA<QuotaExceededException>());
    expect(errorFromResponse(500, env('x'), {}), isA<ServerException>());
    expect(errorFromResponse(503, null, {}), isA<ServerException>());
    final other = errorFromResponse(418, null, {});
    expect(other.runtimeType, ApiException);
    expect(other.code, 'http_error');
    expect(other.message, 'HTTP 418');
  });

  test('carries envelope fields, request id and Retry-After', () {
    final e = errorFromResponse(
      429,
      env('rate_limited'),
      {'retry-after': '3'},
    );
    expect(e.code, 'rate_limited');
    expect(e.message, 'm');
    expect(e.requestId, 'r1');
    expect(e.retryAfter, const Duration(seconds: 3));
    expect(e.status, 429);
  });

  test('falls back to the x-request-id header', () {
    final e = errorFromResponse(500, null, {'x-request-id': 'hdr'});
    expect(e.requestId, 'hdr');
  });

  test('validation exceptions expose per-field errors', () {
    final e = errorFromResponse(
      400,
      env('validation_failed', {
        'fields': {
          'title': {'code': 'required', 'message': 'title is required'},
          'bad': 'ignored',
        },
      }),
      {},
    ) as ValidationException;
    expect(e.fields.keys, ['title']);
    expect(e.fields['title']!.code, 'required');
    expect(e.fields['title']!.message, 'title is required');
  });

  group('parseRetryAfter', () {
    test('delta seconds', () {
      expect(parseRetryAfter('2'), const Duration(seconds: 2));
      expect(parseRetryAfter('0.5'), const Duration(milliseconds: 500));
      expect(parseRetryAfter(' 7 '), const Duration(seconds: 7));
    });

    test('HTTP date', () {
      final now = DateTime.utc(2026, 11, 14, 12);
      expect(
        parseRetryAfter('Sat, 14 Nov 2026 12:00:30 GMT', now: now),
        const Duration(seconds: 30),
      );
      expect(
        parseRetryAfter('Sat, 14 Nov 2026 11:00:00 GMT', now: now),
        Duration.zero,
      );
    });

    test('absent or invalid', () {
      expect(parseRetryAfter(null), isNull);
      expect(parseRetryAfter('soon'), isNull);
      expect(parseRetryAfter('-5'), isNull);
    });
  });
}
