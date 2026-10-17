import Foundation

// Lenient decoding helpers. The protocol promises that unknown fields are
// ignored and optional fields may be missing or null; these helpers also
// tolerate values of the wrong type so a single odd field never drops a whole
// session or transcript.

struct AnyKey: CodingKey {
    var stringValue: String
    var intValue: Int?

    init(_ string: String) {
        stringValue = string
    }

    init?(stringValue: String) {
        self.stringValue = stringValue
    }

    init?(intValue: Int) {
        stringValue = String(intValue)
        self.intValue = intValue
    }
}

typealias Fields = KeyedDecodingContainer<AnyKey>

extension Decoder {
    func fields() throws -> Fields {
        try container(keyedBy: AnyKey.self)
    }
}

extension KeyedDecodingContainer where Key == AnyKey {
    func has(_ key: String) -> Bool {
        contains(AnyKey(key)) && !((try? decodeNil(forKey: AnyKey(key))) ?? true)
    }

    func value<T: Decodable>(_ key: String, as type: T.Type = T.self) -> T? {
        (try? decodeIfPresent(T.self, forKey: AnyKey(key))) ?? nil
    }

    func string(_ key: String) -> String? {
        if let value = value(key, as: String.self) { return value }
        if let value = value(key, as: Int64.self) { return String(value) }
        if let value = value(key, as: Double.self) { return String(value) }
        if let value = value(key, as: Bool.self) { return String(value) }
        return nil
    }

    func int(_ key: String) -> Int? {
        if let value = value(key, as: Int.self) { return value }
        if let value = value(key, as: Double.self), value.isFinite,
            abs(value) < Double(Int.max)
        {
            return Int(value)
        }
        if let value = value(key, as: String.self) {
            return Int(value.trimmingCharacters(in: .whitespaces))
        }
        return nil
    }

    func bool(_ key: String) -> Bool? {
        if let value = value(key, as: Bool.self) { return value }
        if let value = value(key, as: Int.self) { return value != 0 }
        if let value = value(key, as: String.self) {
            switch value.lowercased() {
            case "true", "yes", "1": return true
            case "false", "no", "0": return false
            default: return nil
            }
        }
        return nil
    }

    /// Decodes an array, dropping elements that fail to decode.
    func list<T: Decodable>(_ key: String, of type: T.Type = T.self) -> [T] {
        guard let items = value(key, as: [Lossy<T>].self) else { return [] }
        return items.compactMap(\.value)
    }

    /// Decodes an array of strings, converting numbers and dropping the rest.
    func strings(_ key: String) -> [String] {
        list(key, of: LossyString.self).map(\.value)
    }
}

/// Decodes one array element without failing the array.
struct Lossy<T: Decodable>: Decodable {
    let value: T?

    init(from decoder: Decoder) throws {
        value = try? T(from: decoder)
    }
}

struct LossyString: Decodable {
    let value: String

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if let string = try? container.decode(String.self) {
            value = string
        } else if let number = try? container.decode(Int64.self) {
            value = String(number)
        } else if let object = try? decoder.fields(), let name = object.string("name") ?? object.string("login") {
            value = name
        } else {
            throw DecodingError.dataCorruptedError(in: container, debugDescription: "Not a string")
        }
    }
}

/// A string-backed enum that maps unknown values to a fallback case instead
/// of failing, and matches case-insensitively.
public protocol LenientEnum: RawRepresentable, CaseIterable, Hashable, Sendable
where RawValue == String {
    static var fallback: Self { get }
}

extension LenientEnum {
    public init(lenient raw: String?) {
        guard let raw else {
            self = Self.fallback
            return
        }
        if let exact = Self(rawValue: raw) {
            self = exact
        } else if let loose = Self.allCases.first(where: {
            $0.rawValue.caseInsensitiveCompare(raw) == .orderedSame
        }) {
            self = loose
        } else {
            self = Self.fallback
        }
    }
}

enum Dates {
    /// Parses an ISO-8601 timestamp, with or without fractional seconds.
    static func parse(_ text: String?) -> Date? {
        guard let text, !text.isEmpty else { return nil }
        if let date = try? Date(text, strategy: .iso8601) { return date }
        return try? Date(
            text, strategy: Date.ISO8601FormatStyle(includingFractionalSeconds: true))
    }
}

extension Date {
    /// A date from Unix milliseconds.
    public init(milliseconds: Int) {
        self.init(timeIntervalSince1970: TimeInterval(milliseconds) / 1000)
    }

    public var milliseconds: Int {
        Int((timeIntervalSince1970 * 1000).rounded())
    }
}
