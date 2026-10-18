import Foundation

/// A pull request in a list.
public struct Pr: Identifiable, Hashable, Sendable, Decodable {
    public var repo: String
    public var number: Int
    public var title: String
    public var author: String?
    public var updated: Date?
    public var created: Date?
    public var draft: Bool
    /// `OPEN`, `CLOSED`, `MERGED` (compare case-insensitively).
    public var state: String
    public var conflicts: Bool
    public var reviewers: [String]
    public var additions: Int?
    public var deletions: Int?
    public var changedFiles: Int?
    public var url: URL?

    public var id: String { "\(repo)#\(number)" }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        guard let repo = f.string("repo"), let number = f.int("number") else {
            throw DecodingError.dataCorrupted(
                .init(codingPath: decoder.codingPath, debugDescription: "PR repo/number"))
        }
        self.repo = repo
        self.number = number
        title = f.string("title") ?? ""
        author = f.string("author")
        updated = Dates.parse(f.string("updated"))
        created = Dates.parse(f.string("created"))
        draft = f.bool("draft") ?? false
        state = f.string("state") ?? "OPEN"
        conflicts = f.bool("conflicts") ?? false
        reviewers = f.strings("reviewers")
        additions = f.int("additions")
        deletions = f.int("deletions")
        changedFiles = f.int("changedFiles")
        url = f.string("url").flatMap(URL.init(string:))
    }
}

/// `reviews.inbox`.
public struct Inbox: Hashable, Sendable, Decodable {
    public var viewer: String?
    public var mine: [Pr]
    public var reviewing: [Pr]

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        viewer = f.string("viewer")
        mine = f.list("mine")
        reviewing = f.list("reviewing")
    }
}

public struct ReviewRepository: Identifiable, Hashable, Sendable, Decodable {
    /// `owner/repo`.
    public var name: String
    public var pinned: Bool

    public var id: String { name }

    public init(from decoder: Decoder) throws {
        if let name = try? decoder.singleValueContainer().decode(String.self) {
            self.name = name
            pinned = false
            return
        }
        let f = try decoder.fields()
        guard let name = f.string("name") else {
            throw DecodingError.dataCorrupted(
                .init(codingPath: decoder.codingPath, debugDescription: "repository name"))
        }
        self.name = name
        pinned = f.bool("pinned") ?? false
    }
}

/// `pr.overview`.
public struct PrOverview: Hashable, Sendable, Decodable {
    public var repo: String
    public var number: Int
    public var url: URL?
    public var title: String
    public var body: String
    public var author: String?
    /// `open`, `closed`, `merged` (compare case-insensitively).
    public var state: String
    public var draft: Bool
    /// Head commit SHA; pass back with every write.
    public var head: String
    public var base: String?
    public var headBranch: String?
    public var baseBranch: String?
    public var additions: Int
    public var deletions: Int
    public var changedFiles: Int
    /// `MERGEABLE`, `CONFLICTING`, `UNKNOWN`.
    public var mergeable: String?
    /// `CLEAN`, `BLOCKED`, `BEHIND`, `DIRTY`, `UNSTABLE`, ...
    public var mergeState: String?
    public var reviewers: [PrReviewer]
    public var checks: [PrCheck]
    public var timeline: [TimelineEvent]

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        repo = f.string("repo") ?? ""
        number = f.int("number") ?? 0
        url = f.string("url").flatMap(URL.init(string:))
        title = f.string("title") ?? ""
        body = f.string("body") ?? ""
        author = f.string("author")
        state = f.string("state") ?? "open"
        draft = f.bool("draft") ?? false
        head = f.string("head") ?? ""
        base = f.string("base")
        headBranch = f.string("headBranch")
        baseBranch = f.string("baseBranch")
        additions = f.int("additions") ?? 0
        deletions = f.int("deletions") ?? 0
        changedFiles = f.int("changedFiles") ?? 0
        mergeable = f.string("mergeable")
        mergeState = f.string("mergeState")
        reviewers = f.list("reviewers")
        checks = f.list("checks")
        timeline = f.list("timeline")
    }

    public var isOpen: Bool { state.lowercased() == "open" }
    public var isMerged: Bool { state.lowercased() == "merged" }
}

public struct PrReviewer: Identifiable, Hashable, Sendable, Decodable {
    public var name: String
    /// e.g. `approved`, `changes_requested`, `commented`, `pending`.
    public var state: String

    public var id: String { name }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        name = f.string("name") ?? f.string("login") ?? "?"
        state = f.string("state") ?? "pending"
    }
}

public enum CheckState: String, LenientEnum {
    case pass, fail, pending, skipping, cancelled, expected
    case unknown

    public static let fallback = CheckState.unknown
}

public struct PrCheck: Identifiable, Hashable, Sendable, Decodable {
    public var name: String
    public var state: CheckState
    public var url: URL?

    public var id: String { name + (url?.absoluteString ?? "") }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        name = f.string("name") ?? "Check"
        state = CheckState(lenient: f.string("state"))
        url = f.string("url").flatMap(URL.init(string:))
    }
}

public struct TimelineEvent: Hashable, Sendable, Decodable {
    public var date: Date?
    public var author: String?
    /// e.g. `comment`, `review · approved`, `commit`.
    public var kind: String
    public var body: String
    public var url: URL?

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        date = Dates.parse(f.string("date"))
        author = f.string("author")
        kind = f.string("kind") ?? ""
        body = f.string("body") ?? ""
        url = f.string("url").flatMap(URL.init(string:))
    }
}

/// `{"message": "..."}` returned by PR writes.
struct MessageResult: Decodable {
    var message: String

    init(from decoder: Decoder) throws {
        message = (try? decoder.fields().string("message")) ?? "Done"
    }
}

public enum ReviewEvent: String, Sendable, CaseIterable {
    case approve = "APPROVE"
    case comment = "COMMENT"
    case requestChanges = "REQUEST_CHANGES"
}
