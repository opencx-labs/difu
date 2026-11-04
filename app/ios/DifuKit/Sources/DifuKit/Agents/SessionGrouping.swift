import Foundation

/// A folder with the listed sessions it holds.
public struct FolderGroup<Item> {
    public var folder: SessionFolder
    public var items: [Item]
}

/// The agents list as the terminal arranges it: the orchestrator and every
/// session outside a folder, then each folder in its order. Archived views
/// list only folders holding archived sessions; searches list matches flat.
public enum SessionGrouping {
    /// Arranges one computer's or server's rows. `items` are in display order
    /// and already filtered (active/archived); `archived` is the view.
    public static func arrange<Item>(
        _ items: [Item], folders: [SessionFolder], archived: Bool,
        summary: (Item) -> SessionSummary
    ) -> (loose: [Item], groups: [FolderGroup<Item>]) {
        var home: [String: String] = [:]
        for folder in folders {
            for id in folder.sessions where home[id] == nil {
                home[id] = folder.id
            }
        }
        let known = Set(folders.map(\.id))
        func folderID(_ row: SessionSummary) -> String? {
            guard !row.orchestrator, row.kind != .orchestrator else { return nil }
            // The folder list wins; a summary's own field covers a list that
            // hasn't arrived yet.
            if let id = home[row.id] { return id }
            return row.folder.flatMap { known.contains($0) ? $0 : nil }
        }
        var loose: [Item] = []
        var members: [String: [Item]] = [:]
        for item in items {
            if let id = folderID(summary(item)) {
                members[id, default: []].append(item)
            } else {
                loose.append(item)
            }
        }
        let groups = folders.compactMap { folder -> FolderGroup<Item>? in
            let items = members[folder.id] ?? []
            if archived && items.isEmpty { return nil }
            return FolderGroup(folder: folder, items: items)
        }
        return (loose, groups)
    }

    /// Whether a row matches a search: title, repository (name and path),
    /// branch, id, and `extra` (server and folder names).
    public static func matches(_ summary: SessionSummary, query: String, extra: [String?] = []) -> Bool {
        let query = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else { return true }
        let fields: [String?] =
            [summary.displayTitle, summary.repository, summary.repositoryPath, summary.branch, summary.id]
            + extra
        return fields.compactMap { $0 }.contains { $0.localizedCaseInsensitiveContains(query) }
    }

    /// "3 sessions · 1 working · 2 need input", for a folder row.
    public static func folderDetail(_ sessions: [SessionSummary]) -> String {
        guard !sessions.isEmpty else { return "Empty" }
        var parts = [sessions.count == 1 ? "1 session" : "\(sessions.count) sessions"]
        let working = sessions.filter { $0.active && $0.status != .waiting && $0.pending == 0 }.count
        let input = sessions.filter { $0.pending > 0 || $0.status == .waiting }.count
        if working > 0 { parts.append("\(working) working") }
        if input > 0 { parts.append(input == 1 ? "1 needs input" : "\(input) need input") }
        return parts.joined(separator: " · ")
    }
}
