import 'dart:async';

/// Where the session is kept between app launches.
///
/// The SDK stores the session as one JSON string. Implement this on top of
/// `flutter_secure_storage`, `shared_preferences`, a file or a keychain.
/// Failures are swallowed by the SDK: storage is best effort and a session
/// that cannot be read means "signed out".
abstract interface class TokenStorage {
  /// The stored value, or `null` when nothing is stored.
  FutureOr<String?> read();

  /// Stores [value], replacing any earlier one.
  FutureOr<void> write(String value);

  /// Removes the stored value.
  FutureOr<void> delete();
}

/// Keeps the session in memory: the default; lost when the process ends.
class MemoryTokenStorage implements TokenStorage {
  String? _value;

  @override
  String? read() => _value;

  @override
  void write(String value) => _value = value;

  @override
  void delete() => _value = null;
}

/// Builds a [TokenStorage] from three closures, for quick adapters.
class CallbackTokenStorage implements TokenStorage {
  /// Creates a storage from the given callbacks.
  CallbackTokenStorage({
    required FutureOr<String?> Function() read,
    required FutureOr<void> Function(String value) write,
    required FutureOr<void> Function() delete,
  })  : _read = read,
        _write = write,
        _delete = delete;

  final FutureOr<String?> Function() _read;
  final FutureOr<void> Function(String value) _write;
  final FutureOr<void> Function() _delete;

  @override
  FutureOr<String?> read() => _read();

  @override
  FutureOr<void> write(String value) => _write(value);

  @override
  FutureOr<void> delete() => _delete();
}
