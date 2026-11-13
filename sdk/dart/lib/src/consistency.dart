/// Read-your-writes with read replicas (ADR-0029 §2.6).
///
/// The server answers a write with `X-ForgeBase-Commit: <set>/<lsn>[, …]`.
/// The SDK keeps the highest position per set and sends them back as
/// `X-ForgeBase-Read-After` on later requests, so a replica serves a read only
/// once it has replayed your writes (else the server reads the primary).
/// [Consistency.strong] sends `X-ForgeBase-Consistency: strong`. Servers
/// without replicas send no tokens, and the SDK then sends nothing extra.
library;

/// `session` (default): read your own writes. `strong`: every read on the
/// primary.
enum Consistency {
  /// Read your own writes through the tracked commit tokens.
  session,

  /// Read every call from the primary.
  strong,
}

/// Header carrying commit positions in responses.
const commitHeader = 'X-ForgeBase-Commit';

/// Header carrying the positions a read must have seen.
const readAfterHeader = 'X-ForgeBase-Read-After';

/// Header selecting strong consistency.
const consistencyHeader = 'X-ForgeBase-Consistency';

const _maxSets = 64;
final _setId = RegExp(r'^[A-Za-z0-9_.-]{1,64}$');
final _hex = RegExp(r'^[0-9A-Fa-f]{1,8}$');

({String set, BigInt lsn})? _parseToken(String token) {
  final t = token.trim();
  final slash = t.indexOf('/');
  if (slash <= 0) return null;
  final set = t.substring(0, slash);
  final parts = t.substring(slash + 1).split('/');
  if (!_setId.hasMatch(set) ||
      parts.length != 2 ||
      !_hex.hasMatch(parts[0]) ||
      !_hex.hasMatch(parts[1])) {
    return null;
  }
  final lsn = (BigInt.parse(parts[0], radix: 16) << 32) |
      BigInt.parse(parts[1], radix: 16);
  return (set: set, lsn: lsn);
}

String _formatLsn(BigInt lsn) {
  final hi = (lsn >> 32).toRadixString(16).toUpperCase();
  final lo = (lsn & BigInt.from(0xffffffff)).toRadixString(16).toUpperCase();
  return '$hi/$lo';
}

/// The highest commit position per set this client has seen.
class CommitTokens {
  final Map<String, BigInt> _bySet = {};

  /// Takes the tokens of a header value (`control/16/B374D848, s1/0/9`);
  /// malformed ones are ignored.
  void note(String? header) {
    if (header == null || header.isEmpty) return;
    for (final part in header.split(',')) {
      final t = _parseToken(part);
      if (t == null) continue;
      final held = _bySet[t.set];
      if (held == null && _bySet.length >= _maxSets) continue;
      if (held == null || t.lsn > held) _bySet[t.set] = t.lsn;
    }
  }

  /// The position held for one set (`16/B374D848`).
  String? get(String set) {
    final lsn = _bySet[set];
    return lsn == null ? null : _formatLsn(lsn);
  }

  /// The `X-ForgeBase-Read-After` value, or `null` when nothing was written.
  String? header() {
    if (_bySet.isEmpty) return null;
    final keys = _bySet.keys.toList()..sort();
    return keys.map((s) => '$s/${_formatLsn(_bySet[s]!)}').join(', ');
  }

  /// Forgets every token (after signing out, for example).
  void clear() => _bySet.clear();
}
