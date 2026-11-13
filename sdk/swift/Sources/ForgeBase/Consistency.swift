import Foundation

/// Read-your-writes with read replicas.
///
/// The server answers a write with `X-ForgeBase-Commit: <set>/<lsn>[, ...]`. The SDK keeps the highest
/// position per set and sends them back as `X-ForgeBase-Read-After` on later requests, so a replica serves a
/// read only once it replayed your writes. `.strong` sends `X-ForgeBase-Consistency: strong` and the server
/// reads the primary. Servers without replicas send no tokens and the SDK then sends nothing extra.
public enum Consistency: String, Sendable {
    /// Default: read your own writes.
    case session
    /// Every read on the primary.
    case strong
}

let commitHeader = "X-ForgeBase-Commit"
let readAfterHeader = "X-ForgeBase-Read-After"
let consistencyHeader = "X-ForgeBase-Consistency"

/// The highest commit position per set this client has seen.
public actor CommitTokens {
    private var bySet: [String: UInt64] = [:]
    private static let maxSets = 64

    public init() {}

    private static func parse(_ token: String) -> (set: String, lsn: UInt64)? {
        let t = token.trimmingCharacters(in: .whitespaces)
        guard let slash = t.firstIndex(of: "/"), slash != t.startIndex else { return nil }
        let set = String(t[..<slash])
        let rest = t[t.index(after: slash)...].split(separator: "/", omittingEmptySubsequences: false)
        let setChars = CharacterSet(charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_.-")
        guard set.count <= 64, set.unicodeScalars.allSatisfy(setChars.contains), rest.count == 2,
            rest[0].count >= 1, rest[0].count <= 8, rest[1].count >= 1, rest[1].count <= 8,
            let hi = UInt64(rest[0], radix: 16), let lo = UInt64(rest[1], radix: 16)
        else { return nil }
        return (set, (hi << 32) | lo)
    }

    private static func format(_ lsn: UInt64) -> String {
        "\(String(lsn >> 32, radix: 16).uppercased())/\(String(lsn & 0xFFFF_FFFF, radix: 16).uppercased())"
    }

    /// Takes the tokens of a header value (`control/16/B374D848, s1/0/9`); malformed ones are ignored.
    public func note(_ header: String?) {
        guard let header, !header.isEmpty else { return }
        for part in header.split(separator: ",") {
            guard let t = Self.parse(String(part)) else { continue }
            let held = bySet[t.set]
            if held == nil && bySet.count >= Self.maxSets { continue }
            if held == nil || t.lsn > held! { bySet[t.set] = t.lsn }
        }
    }

    /// The position held for one set (`16/B374D848`).
    public func get(_ set: String) -> String? {
        bySet[set].map(Self.format)
    }

    /// The `X-ForgeBase-Read-After` value, or `nil` when nothing was written yet.
    public func header() -> String? {
        if bySet.isEmpty { return nil }
        return bySet.sorted { $0.key < $1.key }.map { "\($0.key)/\(Self.format($0.value))" }.joined(separator: ", ")
    }

    /// Forgets every token (after signing out, for example).
    public func clear() { bySet.removeAll() }
}
