import Foundation

/// One transcript row, already normalized for display by the server.
public struct EntryView: Identifiable, Hashable, Sendable {
    public var id: String
    /// Raw provider kind such as `commandExecution` or `fileChange` (for icons).
    public var kind: String
    public var role: EntryRole
    public var text: String
    public var title: String?
    public var detail: String?
    /// Tools only.
    public var state: ToolState?
    public var startedAt: Int?
    public var finishedAt: Int?
    public var files: [EntryFile]
    public var plan: [PlanStep]
    /// User rows: nil once delivered.
    public var delivery: Delivery?
    /// Text or diff was shortened; fetch the full row with `session.entry`.
    public var truncated: Bool

    public init(
        id: String, kind: String = "", role: EntryRole, text: String = "",
        title: String? = nil, detail: String? = nil, state: ToolState? = nil,
        startedAt: Int? = nil, finishedAt: Int? = nil, files: [EntryFile] = [],
        plan: [PlanStep] = [], delivery: Delivery? = nil, truncated: Bool = false
    ) {
        self.id = id
        self.kind = kind
        self.role = role
        self.text = text
        self.title = title
        self.detail = detail
        self.state = state
        self.startedAt = startedAt
        self.finishedAt = finishedAt
        self.files = files
        self.plan = plan
        self.delivery = delivery
        self.truncated = truncated
    }

    /// Milliseconds between start and finish (or `now` while running).
    public func duration(now: Int) -> Int? {
        guard let startedAt else { return nil }
        let end = finishedAt ?? (state == .running ? now : nil)
        guard let end else { return nil }
        return max(0, end - startedAt)
    }
}

extension EntryView: Decodable {
    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        id = f.string("id") ?? ""
        kind = f.string("kind") ?? ""
        role = EntryRole(lenient: f.string("role"))
        text = f.string("text") ?? ""
        title = f.string("title")
        detail = f.string("detail")
        state = f.string("state").map(ToolState.init(lenient:))
        startedAt = f.int("startedAt")
        finishedAt = f.int("finishedAt")
        files = f.list("files")
        plan = f.list("plan")
        delivery = f.string("delivery").map(Delivery.init(lenient:))
        truncated = f.bool("truncated") ?? false
    }
}

/// Decodes an entry, substituting a placeholder for malformed rows.
struct EntryOrPlaceholder: Decodable {
    let entry: EntryView

    init(from decoder: Decoder) throws {
        entry = (try? EntryView(from: decoder))
            ?? EntryView(id: "", role: .system, text: "(This row could not be displayed.)")
    }
}

public struct EntryFile: Hashable, Sendable, Decodable {
    public var path: String
    public var additions: Int
    public var deletions: Int
    public var diff: String

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        path = f.string("path") ?? ""
        diff = f.string("diff") ?? f.string("patch") ?? ""
        let counted = Patch.stats(diff)
        additions = f.int("additions") ?? counted.additions
        deletions = f.int("deletions") ?? counted.deletions
    }
}

public struct PlanStep: Hashable, Sendable, Decodable {
    public var step: String
    public var status: PlanStatus

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        step = f.string("step") ?? ""
        status = PlanStatus(lenient: f.string("status"))
    }
}
