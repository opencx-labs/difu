import Foundation

/// A generated review guide for a PR (`pr.guide`, `pr.generateGuide`).
public struct Guide: Hashable, Sendable, Decodable {
    public enum Status: String, LenientEnum {
        case notGenerated = "none"
        case running, ready, failed
        case unknown

        public static let fallback = Status.unknown
    }

    public var status: Status
    /// Progress while running, the error when failed.
    public var message: String?
    /// The guide job's session id, when one exists.
    public var session: String?
    /// Generated for an older head commit.
    public var outdated: Bool
    public var chapters: [GuideChapter]

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        status = Status(lenient: f.string("status"))
        message = f.string("message").flatMap { $0.isEmpty ? nil : $0 }
        session = f.string("session")
        outdated = f.bool("outdated") ?? false
        chapters = f.list("chapters")
    }
}

public struct GuideChapter: Hashable, Sendable, Decodable {
    public enum Category: String, LenientEnum {
        case schema, migrations, regular, generated, tests

        public static let fallback = Category.regular

        /// Display order of chapter groups.
        public var order: Int {
            switch self {
            case .schema: 0
            case .migrations: 1
            case .regular: 2
            case .generated: 3
            case .tests: 4
            }
        }

        public var title: String {
            switch self {
            case .schema: "Schema"
            case .migrations: "Migrations"
            case .regular: "Changes"
            case .generated: "Generated"
            case .tests: "Tests"
            }
        }
    }

    public var category: Category
    public var title: String
    public var explanation: String
    public var hunks: [GuideHunk]

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        category = Category(lenient: f.string("category"))
        title = f.string("title") ?? ""
        explanation = f.string("explanation") ?? ""
        hunks = f.list("hunks")
    }
}

public struct GuideHunk: Hashable, Sendable, Decodable {
    public var file: String
    public var header: String
    public var lines: [GuideLine]

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        file = f.string("file") ?? ""
        header = f.string("header") ?? ""
        lines = f.list("lines")
    }
}

public struct GuideLine: Hashable, Sendable, Decodable {
    public enum Kind: String, LenientEnum {
        case context, add, remove, meta

        public static let fallback = Kind.context
    }

    public var kind: Kind
    public var old: Int?
    public var new: Int?
    public var text: String

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        kind = Kind(lenient: f.string("kind"))
        old = f.int("old")
        new = f.int("new")
        text = f.string("text") ?? ""
    }
}
