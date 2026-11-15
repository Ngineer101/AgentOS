import Foundation

/// Any JSON value. Event payloads and tool arguments are kept in this form so
/// fields the app does not know about survive caching unchanged.
enum JSONValue: Codable, Hashable {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case array([JSONValue])
    case object([String: JSONValue])

    init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() {
            self = .null
        } else if let b = try? c.decode(Bool.self) {
            self = .bool(b)
        } else if let n = try? c.decode(Double.self) {
            self = .number(n)
        } else if let s = try? c.decode(String.self) {
            self = .string(s)
        } else if let a = try? c.decode([JSONValue].self) {
            self = .array(a)
        } else if let o = try? c.decode([String: JSONValue].self) {
            self = .object(o)
        } else {
            throw DecodingError.dataCorruptedError(in: c, debugDescription: "not JSON")
        }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .null: try c.encodeNil()
        case .bool(let b): try c.encode(b)
        case .number(let n): try c.encode(n)
        case .string(let s): try c.encode(s)
        case .array(let a): try c.encode(a)
        case .object(let o): try c.encode(o)
        }
    }

    subscript(key: String) -> JSONValue {
        if case .object(let o) = self, let v = o[key] { return v }
        return .null
    }

    var string: String? {
        if case .string(let s) = self { return s }
        return nil
    }

    var bool: Bool? {
        if case .bool(let b) = self { return b }
        return nil
    }

    var int: Int64? {
        if case .number(let n) = self { return Int64(n) }
        return nil
    }

    var isNull: Bool {
        if case .null = self { return true }
        return false
    }

    /// Human-readable text: strings as-is, everything else as pretty JSON.
    var displayText: String {
        if case .string(let s) = self { return s }
        if case .null = self { return "" }
        return prettyJSON
    }

    var prettyJSON: String {
        guard let data = try? JSONEncoder.pretty.encode(self),
              let s = String(data: data, encoding: .utf8) else { return "" }
        return s
    }

    /// Decode this value as a typed model (for example an approval payload).
    func decode<T: Decodable>(_ type: T.Type) -> T? {
        guard let data = try? JSONEncoder().encode(self) else { return nil }
        return try? JSONDecoder().decode(T.self, from: data)
    }
}

extension JSONEncoder {
    static let pretty: JSONEncoder = {
        let e = JSONEncoder()
        e.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        return e
    }()
}

/// Build JSON-RPC params as text. Values must be JSON-compatible
/// (String, Bool, numbers, arrays and dictionaries of those, NSNull).
func jsonText(_ object: [String: Any]) -> String {
    guard JSONSerialization.isValidJSONObject(object),
          let data = try? JSONSerialization.data(withJSONObject: object),
          let s = String(data: data, encoding: .utf8) else { return "{}" }
    return s
}
