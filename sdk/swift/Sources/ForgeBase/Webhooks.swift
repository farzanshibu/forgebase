import CryptoKit
import Foundation

/// Webhook verification for push subscriptions.
///
/// The server POSTs `{subscription, delivery_attempt, message}` as JSON with
/// `X-Forgebase-Signature: t=<unix seconds>,v1=<hex HMAC-SHA256(secret, "<t>.<raw body>")>`. The key is the UTF-8 text of
/// the subscription's secret (`whsec_...`, prefix included). While a secret is being rotated the header may carry
/// several `v1=` values. Receiving webhooks needs a server (a Swift-on-server app, a function); the helper is here so
/// such code does not have to hand-roll HMAC.
public enum Webhook {
    /// Header the server signs push requests with.
    public static let signatureHeader = "X-Forgebase-Signature"
    /// Default accepted clock distance between the signed timestamp and `now`, in seconds.
    public static let defaultTolerance: TimeInterval = 300

    public enum Failure: String, Sendable {
        case missingHeader = "missing_header"
        case malformedHeader = "malformed_header"
        case timestampOutOfTolerance = "timestamp_out_of_tolerance"
        case signatureMismatch = "signature_mismatch"
        case invalidPayload = "invalid_payload"
    }

    /// The signature header for `body` signed at `timestamp` (tests, local simulation).
    public static func sign(secret: String, body: Data, timestamp: Int = Int(Date().timeIntervalSince1970)) -> String {
        "t=\(timestamp),v1=\(hmac(secret: secret, timestamp: timestamp, body: body))"
    }

    /// `true` when the signature is valid; `construct` says why when it is not.
    public static func verify(secrets: [String], header: String?, body: Data, tolerance: TimeInterval = defaultTolerance, now: Date = Date()) -> Bool {
        failure(secrets: secrets, header: header, body: body, tolerance: tolerance, now: now) == nil
    }

    /// Verifies the request, then returns the parsed push envelope. Throws a `.webhook` error (code `webhook_<reason>`) when
    /// the signature, the timestamp or the payload is not acceptable; answer such a request with a 4xx.
    public static func construct(secrets: [String], header: String?, body: Data, tolerance: TimeInterval = defaultTolerance, now: Date = Date()) throws -> JSON {
        if let f = failure(secrets: secrets, header: header, body: body, tolerance: tolerance, now: now) { throw error(f) }
        guard let j = try? JSON(data: body), j["message"] != nil else { throw error(.invalidPayload) }
        return j
    }

    private static func error(_ f: Failure) -> ForgeBaseError {
        ForgeBaseError(kind: .webhook, code: "webhook_\(f.rawValue)", message: "webhook verification failed: \(f.rawValue)")
    }

    static func failure(secrets: [String], header: String?, body: Data, tolerance: TimeInterval, now: Date) -> Failure? {
        guard let header, !header.isEmpty else { return .missingHeader }
        var timestamp: Int?
        var signatures: [String] = []
        for part in header.split(separator: ",") {
            let kv = part.split(separator: "=", maxSplits: 1).map { $0.trimmingCharacters(in: .whitespaces) }
            guard kv.count == 2 else { return .malformedHeader }
            if kv[0] == "t" { timestamp = Int(kv[1]) } else if kv[0] == "v1" { signatures.append(kv[1].lowercased()) }
        }
        guard let timestamp, !signatures.isEmpty else { return .malformedHeader }
        if tolerance.isFinite, abs(now.timeIntervalSince1970 - Double(timestamp)) > tolerance { return .timestampOutOfTolerance }
        for secret in secrets {
            let expected = hmac(secret: secret, timestamp: timestamp, body: body)
            if signatures.contains(where: { constantTimeEqual($0, expected) }) { return nil }
        }
        return .signatureMismatch
    }

    private static func hmac(secret: String, timestamp: Int, body: Data) -> String {
        var message = Data("\(timestamp).".utf8)
        message.append(body)
        let mac = HMAC<SHA256>.authenticationCode(for: message, using: SymmetricKey(data: Data(secret.utf8)))
        return mac.map { String(format: "%02x", $0) }.joined()
    }

    private static func constantTimeEqual(_ a: String, _ b: String) -> Bool {
        let x = Array(a.utf8), y = Array(b.utf8)
        guard x.count == y.count else { return false }
        var diff: UInt8 = 0
        for i in 0..<x.count { diff |= x[i] ^ y[i] }
        return diff == 0
    }
}
