import Foundation

/// A pull request attached to a session or discovered from its branch
/// (`SessionSummary.prs`). Open it with `pr.overview` / `pr.diff` on the same
/// computer or server.
public struct SessionPr: Identifiable, Hashable, Sendable {
    /// `owner/repo`.
    public var repo: String
    public var number: Int
    public var title: String?
    /// `OPEN`, `MERGED` or `CLOSED`; empty until GitHub was asked.
    public var state: String
    /// Display-ready: Open, Draft, Has conflicts, Merged, Closed or Unknown.
    public var label: String
    public var draft: Bool
    public var conflicts: Bool
    /// Explicitly attached (possibly from another repository) rather than
    /// discovered from the session's branch.
    public var attached: Bool
    public var url: String?
    public var headBranch: String?

    public var id: String { "\(repo)#\(number)" }

    /// The repository name without its owner.
    public var repoName: String {
        repo.split(separator: "/").last.map(String.init) ?? repo
    }
}

extension SessionPr: Decodable {
    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        guard let repo = f.string("repo"), !repo.isEmpty, let number = f.int("number") else {
            throw DecodingError.dataCorrupted(
                .init(codingPath: decoder.codingPath, debugDescription: "Session PR needs repo and number"))
        }
        self.repo = repo
        self.number = number
        title = f.string("title").flatMap { $0.isEmpty ? nil : $0 }
        state = f.string("state") ?? ""
        draft = f.bool("draft") ?? false
        conflicts = f.bool("conflicts") ?? false
        label = f.string("label").flatMap { $0.isEmpty ? nil : $0 }
            ?? Self.label(state: state, draft: draft, conflicts: conflicts)
        attached = f.bool("attached") ?? false
        url = f.string("url").flatMap { $0.isEmpty ? nil : $0 }
        headBranch = f.string("headBranch").flatMap { $0.isEmpty ? nil : $0 }
    }

    /// The terminal's badge label, for services that send no `label`.
    static func label(state: String, draft: Bool, conflicts: Bool) -> String {
        switch state.uppercased() {
        case "MERGED": "Merged"
        case "CLOSED": "Closed"
        case "OPEN" where conflicts: "Has conflicts"
        case "OPEN" where draft: "Draft"
        case "OPEN": "Open"
        default: "Unknown"
        }
    }
}
