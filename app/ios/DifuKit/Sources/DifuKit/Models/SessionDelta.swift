import Foundation

/// An incremental update to a watched session.
public struct SessionDelta: Sendable, Equatable, Decodable {
    public struct Change: Sendable, Equatable {
        public var index: Int
        public var entry: EntryView
    }

    public var id: String
    /// The version this delta applies on top of.
    public var base: Int
    /// Every SessionView field except `entries` (including the new `version`).
    public var session: SessionView?
    /// The transcript length after applying the delta.
    public var count: Int?
    public var entries: [Change]

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        session = f.value("session", as: SessionView.self)
        guard let id = f.string("id") ?? session?.id else {
            throw DecodingError.keyNotFound(
                AnyKey("id"), .init(codingPath: decoder.codingPath, debugDescription: "delta id"))
        }
        self.id = id
        if session == nil, case .object(var fields)? = f.value("session", as: JSONValue.self) {
            // Tolerate a `session` object that omits its id.
            fields["id"] = .string(id)
            session = try? JSONDecoder().decode(
                SessionView.self, from: Data(JSONValue.object(fields).jsonString().utf8))
        }
        base = f.int("base") ?? -1
        count = f.int("count")
        entries = f.list("entries", of: IndexedEntry.self).map {
            Change(index: $0.index, entry: $0.entry)
        }
    }
}

/// `[index, EntryView]` (or `{"index": .., "entry": ..}`).
private struct IndexedEntry: Decodable {
    var index: Int
    var entry: EntryView

    init(from decoder: Decoder) throws {
        if var pair = try? decoder.unkeyedContainer() {
            index = try pair.decode(Int.self)
            entry = try pair.decode(EntryOrPlaceholder.self).entry
        } else {
            let f = try decoder.fields()
            guard let index = f.int("index"), let entry = f.value("entry", as: EntryView.self)
            else {
                throw DecodingError.dataCorrupted(
                    .init(codingPath: decoder.codingPath, debugDescription: "delta entry"))
            }
            self.index = index
            self.entry = entry
        }
    }
}

extension SessionView {
    public enum DeltaOutcome: Equatable, Sendable {
        case applied
        /// The delta doesn't line up with this view: watch the session again.
        case needsRewatch
    }

    /// Applies `delta` in place. On `.needsRewatch` the view is left unchanged.
    @discardableResult
    public mutating func apply(_ delta: SessionDelta) -> DeltaOutcome {
        guard delta.id == id, delta.base == version else { return .needsRewatch }
        let count = delta.count
            ?? max(entries.count, (delta.entries.map(\.index).max() ?? -1) + 1)
        let changes = delta.entries
            .filter { $0.index >= 0 && $0.index < count }
            .sorted { $0.index < $1.index }

        // Validate first so a mismatch leaves the view untouched.
        var length = min(entries.count, count)
        for change in changes where change.index >= length {
            guard change.index == length else { return .needsRewatch }
            length += 1
        }
        guard length == count else { return .needsRewatch }

        var entries = self.entries
        self.entries = []  // keep the array uniquely referenced while editing
        if entries.count > count {
            entries.removeSubrange(count...)
        }
        for change in changes {
            if change.index < entries.count {
                entries[change.index] = change.entry
            } else {
                entries.append(change.entry)
            }
        }
        if var next = delta.session {
            next.entries = entries
            self = next
        } else {
            self.entries = entries
        }
        return .applied
    }
}
