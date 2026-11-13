import Foundation
#if canImport(Security)
import Security
#endif

/// Where the signed-in session lives between launches. Implement it to keep sessions in your own storage.
public protocol TokenStore: Sendable {
    func load() async throws -> Session?
    func save(_ session: Session) async throws
    func clear() async throws
}

/// Keeps the session in memory only (the default): a relaunch signs the user out.
public actor InMemoryTokenStore: TokenStore {
    private var session: Session?
    public init(_ session: Session? = nil) { self.session = session }
    public func load() async throws -> Session? { session }
    public func save(_ session: Session) async throws { self.session = session }
    public func clear() async throws { session = nil }
}

#if canImport(Security)
/// Keeps the session in the Keychain as one generic-password item (JSON encoded).
///
///     let store = KeychainTokenStore(service: "com.example.myapp.forgebase")
///     let fb = ForgeBaseClient(baseURL: url, tokenStore: store)
///
/// Share a session between an app and its extensions with `accessGroup` (needs the Keychain Sharing capability).
public struct KeychainTokenStore: TokenStore {
    public enum Accessibility: Sendable {
        /// Readable after the first unlock, never leaves the device (default: works for background refresh).
        case afterFirstUnlockThisDeviceOnly
        case whenUnlockedThisDeviceOnly
        case afterFirstUnlock
        case whenUnlocked

        var value: CFString {
            switch self {
            case .afterFirstUnlockThisDeviceOnly: return kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
            case .whenUnlockedThisDeviceOnly: return kSecAttrAccessibleWhenUnlockedThisDeviceOnly
            case .afterFirstUnlock: return kSecAttrAccessibleAfterFirstUnlock
            case .whenUnlocked: return kSecAttrAccessibleWhenUnlocked
            }
        }
    }

    public let service: String
    public let account: String
    public let accessGroup: String?
    public let accessibility: Accessibility

    public init(
        service: String = "forgebase.session", account: String = "default", accessGroup: String? = nil,
        accessibility: Accessibility = .afterFirstUnlockThisDeviceOnly
    ) {
        self.service = service
        self.account = account
        self.accessGroup = accessGroup
        self.accessibility = accessibility
    }

    private var baseQuery: [String: Any] {
        var q: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        if let accessGroup { q[kSecAttrAccessGroup as String] = accessGroup }
        return q
    }

    public func load() async throws -> Session? {
        var q = baseQuery
        q[kSecReturnData as String] = true
        q[kSecMatchLimit as String] = kSecMatchLimitOne
        var out: AnyObject?
        let status = SecItemCopyMatching(q as CFDictionary, &out)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = out as? Data else { throw KeychainTokenStore.error(status) }
        return try? JSONDecoder().decode(Session.self, from: data)
    }

    public func save(_ session: Session) async throws {
        let data = try JSONEncoder().encode(session)
        let update: [String: Any] = [kSecValueData as String: data]
        var status = SecItemUpdate(baseQuery as CFDictionary, update as CFDictionary)
        if status == errSecItemNotFound {
            var add = baseQuery
            add[kSecValueData as String] = data
            add[kSecAttrAccessible as String] = accessibility.value
            status = SecItemAdd(add as CFDictionary, nil)
        }
        guard status == errSecSuccess else { throw KeychainTokenStore.error(status) }
    }

    public func clear() async throws {
        let status = SecItemDelete(baseQuery as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else { throw KeychainTokenStore.error(status) }
    }

    private static func error(_ status: OSStatus) -> NSError {
        let message = SecCopyErrorMessageString(status, nil) as String? ?? "Keychain error"
        return NSError(domain: NSOSStatusErrorDomain, code: Int(status), userInfo: [NSLocalizedDescriptionKey: message])
    }
}
#endif
