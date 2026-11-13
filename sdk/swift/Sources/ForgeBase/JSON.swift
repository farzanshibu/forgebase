import Foundation

/// A JSON value. Used wherever the server's shape is open-ended: record fields, metadata, function
/// payloads, query results. Typed wire objects (sessions, objects, uploads, ...) are plain `Codable` structs.
///
///     let post = try await fb.collection("posts").get("abc")      // JSONObject
///     post["title"]?.stringValue                                    // "Hello"
///     post.title                                                    // dynamic member lookup on JSON
@dynamicMemberLookup
public enum JSON: Sendable, Hashable {
    case null
    case bool(Bool)
    case int(Int64)
    case double(Double)
    case string(String)
    case array([JSON])
    case object([String: JSON])
}

/// A JSON object, the default shape of a record.
public typealias JSONObject = [String: JSON]

extension JSON: Codable {
    public init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() {
            self = .null
        } else if let v = try? c.decode(Bool.self) {
            self = .bool(v)
        } else if let v = try? c.decode(Int64.self) {
            self = .int(v)
        } else if let v = try? c.decode(Double.self) {
            self = .double(v)
        } else if let v = try? c.decode(String.self) {
            self = .string(v)
        } else if let v = try? c.decode([JSON].self) {
            self = .array(v)
        } else if let v = try? c.decode([String: JSON].self) {
            self = .object(v)
        } else {
            throw DecodingError.dataCorruptedError(in: c, debugDescription: "not a JSON value")
        }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .null: try c.encodeNil()
        case .bool(let v): try c.encode(v)
        case .int(let v): try c.encode(v)
        case .double(let v): try c.encode(v)
        case .string(let v): try c.encode(v)
        case .array(let v): try c.encode(v)
        case .object(let v): try c.encode(v)
        }
    }
}

extension JSON: ExpressibleByNilLiteral, ExpressibleByBooleanLiteral, ExpressibleByIntegerLiteral,
    ExpressibleByFloatLiteral, ExpressibleByStringLiteral, ExpressibleByArrayLiteral, ExpressibleByDictionaryLiteral
{
    public init(nilLiteral: ()) { self = .null }
    public init(booleanLiteral value: Bool) { self = .bool(value) }
    public init(integerLiteral value: Int64) { self = .int(value) }
    public init(floatLiteral value: Double) { self = .double(value) }
    public init(stringLiteral value: String) { self = .string(value) }
    public init(arrayLiteral elements: JSON...) { self = .array(elements) }
    public init(dictionaryLiteral elements: (String, JSON)...) {
        self = .object(Dictionary(elements, uniquingKeysWith: { _, last in last }))
    }
}

extension JSON {
    /// Encodes any `Encodable` into a JSON value.
    public init<T: Encodable>(encoding value: T) throws {
        if let json = value as? JSON {
            self = json
            return
        }
        let data = try JSONEncoder().encode(value)
        self = try JSONDecoder().decode(JSON.self, from: data)
    }

    /// Parses JSON text or bytes.
    public init(data: Data) throws {
        self = try JSONDecoder().decode(JSON.self, from: data)
    }

    /// Decodes the value into a `Decodable` type (plain keys, no key conversion).
    public func decode<T: Decodable>(_ type: T.Type = T.self) throws -> T {
        if let same = self as? T { return same }
        let data = try JSONEncoder().encode(self)
        do {
            return try JSONDecoder().decode(T.self, from: data)
        } catch {
            throw ForgeBaseError.invalidResponse("could not decode \(T.self): \(error)", status: 200)
        }
    }

    /// Compact UTF-8 JSON.
    public func data() throws -> Data { try JSONEncoder().encode(self) }

    public var isNull: Bool { if case .null = self { return true } else { return false } }
    public var boolValue: Bool? { if case .bool(let v) = self { return v } else { return nil } }
    public var stringValue: String? { if case .string(let v) = self { return v } else { return nil } }
    public var arrayValue: [JSON]? { if case .array(let v) = self { return v } else { return nil } }
    public var objectValue: [String: JSON]? { if case .object(let v) = self { return v } else { return nil } }
    public var intValue: Int? {
        switch self {
        case .int(let v): return Int(exactly: v)
        case .double(let v): return Int(exactly: v)
        default: return nil
        }
    }
    public var int64Value: Int64? {
        switch self {
        case .int(let v): return v
        case .double(let v): return Int64(exactly: v)
        default: return nil
        }
    }
    public var doubleValue: Double? {
        switch self {
        case .int(let v): return Double(v)
        case .double(let v): return v
        default: return nil
        }
    }

    public subscript(key: String) -> JSON? {
        if case .object(let o) = self { return o[key] }
        return nil
    }

    public subscript(index: Int) -> JSON? {
        if case .array(let a) = self, a.indices.contains(index) { return a[index] }
        return nil
    }

    public subscript(dynamicMember key: String) -> JSON? { self[key] }
}

extension JSON: CustomStringConvertible {
    public var description: String {
        guard let data = try? JSONEncoder().encode(self), let s = String(data: data, encoding: .utf8) else { return "null" }
        return s
    }
}

// MARK: - building request bodies and queries

/// Something that can appear in a request body. `nil` optionals are left out.
public protocol JSONConvertible: Sendable {
    var jsonValue: JSON? { get }
}

extension JSON: JSONConvertible { public var jsonValue: JSON? { self } }
extension String: JSONConvertible { public var jsonValue: JSON? { .string(self) } }
extension Bool: JSONConvertible { public var jsonValue: JSON? { .bool(self) } }
extension Int: JSONConvertible { public var jsonValue: JSON? { .int(Int64(self)) } }
extension Int64: JSONConvertible { public var jsonValue: JSON? { .int(self) } }
extension Double: JSONConvertible { public var jsonValue: JSON? { .double(self) } }
extension Date: JSONConvertible {
    public var jsonValue: JSON? { .string(ISO8601DateFormatter.forgebaseString(self)) }
}
extension Optional: JSONConvertible where Wrapped: JSONConvertible {
    public var jsonValue: JSON? { self?.jsonValue }
}
extension Array: JSONConvertible where Element: JSONConvertible {
    public var jsonValue: JSON? { .array(compactMap(\.jsonValue)) }
}
extension Dictionary: JSONConvertible where Key == String, Value: JSONConvertible {
    public var jsonValue: JSON? { .object(compactMapValues(\.jsonValue)) }
}

extension ISO8601DateFormatter {
    /// RFC 3339 with fractional seconds (a formatter per call: `ISO8601DateFormatter` is not `Sendable`).
    static func forgebaseString(_ date: Date) -> String {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f.string(from: date)
    }
}

/// A JSON object from key/value pairs, skipping `nil` values: `wire(["name": name, "size": size])`.
func wire(_ pairs: KeyValuePairs<String, (any JSONConvertible)?>) -> JSON {
    var out: [String: JSON] = [:]
    for (k, v) in pairs {
        if let j = v?.jsonValue { out[k] = j }
    }
    return .object(out)
}

/// A value that can appear in a query string.
public protocol QueryConvertible: Sendable {
    var queryValue: String { get }
}
extension String: QueryConvertible { public var queryValue: String { self } }
extension Int: QueryConvertible { public var queryValue: String { String(self) } }
extension Int64: QueryConvertible { public var queryValue: String { String(self) } }
extension Double: QueryConvertible { public var queryValue: String { String(self) } }
extension Bool: QueryConvertible { public var queryValue: String { self ? "true" : "false" } }

func query(_ pairs: KeyValuePairs<String, (any QueryConvertible)?>) -> [URLQueryItem] {
    pairs.compactMap { k, v in v.map { URLQueryItem(name: k, value: $0.queryValue) } }
}
