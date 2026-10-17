import XCTest

@testable import DifuKit

final class SessionDeltaTests: XCTestCase {
    func makeView(version: Int = 10, entries: [String]) throws -> SessionView {
        let rows = entries.map { #"{"id": "\#($0)", "role": "assistant", "text": "\#($0)"}"# }
        return try decode(
            SessionView.self,
            #"{"id": "s", "title": "Old", "status": "Running", "version": \#(version), "entries": [\#(rows.joined(separator: ","))]}"#
        )
    }

    func makeDelta(
        base: Int = 10, version: Int = 11, count: Int?, changes: [(Int, String)],
        title: String = "New", includeSession: Bool = true
    ) throws -> SessionDelta {
        let pairs = changes.map { #"[\#($0.0), {"id": "\#($0.1)", "role": "tool", "text": "\#($0.1)"}]"# }
        let session = includeSession
            ? #""session": {"id": "s", "title": "\#(title)", "status": "Idle", "version": \#(version), "canSend": false},"#
            : ""
        let countField = count.map { #""count": \#($0),"# } ?? ""
        return try decode(
            SessionDelta.self,
            #"{"id": "s", "base": \#(base), \#(session) \#(countField) "entries": [\#(pairs.joined(separator: ","))]}"#
        )
    }

    func testAppendsAndReplacesOutOfOrderPairs() throws {
        var view = try makeView(entries: ["a", "b", "c"])
        let delta = try makeDelta(count: 5, changes: [(4, "e"), (2, "c2"), (3, "d")])
        XCTAssertEqual(view.apply(delta), .applied)
        XCTAssertEqual(view.entries.map(\.id), ["a", "b", "c2", "d", "e"])
        XCTAssertEqual(view.entries[3].role, .tool)
    }

    func testReplacesOtherFieldsIncludingVersion() throws {
        var view = try makeView(entries: ["a"])
        XCTAssertEqual(view.apply(try makeDelta(count: 1, changes: [])), .applied)
        XCTAssertEqual(view.version, 11)
        XCTAssertEqual(view.title, "New")
        XCTAssertEqual(view.status, .idle)
        XCTAssertFalse(view.canSend)
        XCTAssertEqual(view.entries.map(\.id), ["a"], "entries survive a delta without entries")
    }

    func testTruncates() throws {
        var view = try makeView(entries: ["a", "b", "c", "d"])
        XCTAssertEqual(view.apply(try makeDelta(count: 2, changes: [(1, "b2")])), .applied)
        XCTAssertEqual(view.entries.map(\.id), ["a", "b2"])

        var cleared = try makeView(entries: ["a", "b"])
        XCTAssertEqual(cleared.apply(try makeDelta(count: 0, changes: [])), .applied)
        XCTAssertTrue(cleared.entries.isEmpty)
    }

    func testTruncateThenAppend() throws {
        var view = try makeView(entries: ["a", "b", "c"])
        XCTAssertEqual(view.apply(try makeDelta(count: 3, changes: [(1, "x"), (2, "y")])), .applied)
        XCTAssertEqual(view.entries.map(\.id), ["a", "x", "y"])
    }

    func testBaseMismatchLeavesViewUntouched() throws {
        var view = try makeView(entries: ["a", "b"])
        let before = view
        XCTAssertEqual(view.apply(try makeDelta(base: 9, count: 3, changes: [(2, "c")])), .needsRewatch)
        XCTAssertEqual(view, before)
    }

    func testGapRequiresRewatch() throws {
        var view = try makeView(entries: ["a"])
        let before = view
        XCTAssertEqual(view.apply(try makeDelta(count: 4, changes: [(3, "d")])), .needsRewatch)
        XCTAssertEqual(view, before)
    }

    func testMissingRowsRequireRewatch() throws {
        var view = try makeView(entries: ["a"])
        XCTAssertEqual(view.apply(try makeDelta(count: 3, changes: [(1, "b")])), .needsRewatch)
        XCTAssertEqual(view.entries.map(\.id), ["a"])
    }

    func testIgnoresPairsBeyondCount() throws {
        var view = try makeView(entries: ["a"])
        XCTAssertEqual(view.apply(try makeDelta(count: 2, changes: [(1, "b"), (7, "zz")])), .applied)
        XCTAssertEqual(view.entries.map(\.id), ["a", "b"])
    }

    func testDeltaWithoutSessionOrCount() throws {
        var view = try makeView(entries: ["a"])
        let delta = try makeDelta(count: nil, changes: [(1, "b")], includeSession: false)
        XCTAssertNil(delta.session)
        XCTAssertEqual(view.apply(delta), .applied)
        XCTAssertEqual(view.entries.map(\.id), ["a", "b"])
        XCTAssertEqual(view.version, 10, "version only changes with a session payload")
    }

    func testSessionPayloadWithoutId() throws {
        let delta = try decode(
            SessionDelta.self,
            #"{"id": "s", "base": 10, "session": {"title": "Renamed", "version": 11}, "count": 0, "entries": []}"#)
        XCTAssertEqual(delta.session?.id, "s")
        var view = try makeView(entries: [])
        XCTAssertEqual(view.apply(delta), .applied)
        XCTAssertEqual(view.title, "Renamed")
        XCTAssertEqual(view.version, 11)
    }

    func testWrongSessionId() throws {
        var view = try makeView(entries: [])
        let delta = try decode(SessionDelta.self, #"{"id": "other", "base": 10, "count": 0, "entries": []}"#)
        XCTAssertEqual(view.apply(delta), .needsRewatch)
    }

    func testDeltaEventFrame() throws {
        let frame = try decode(
            IncomingFrame.self,
            #"{"event": "session.delta", "data": {"id": "s", "base": 411, "session": {"id": "s", "version": 412}, "count": 58, "entries": [[57, {"id": "e57", "role": "assistant"}], [56, {"id": "e56", "role": "user"}]]}}"#
        )
        guard case .sessionDelta(let delta)? = frame.event else { return XCTFail("no delta") }
        XCTAssertEqual(delta.base, 411)
        XCTAssertEqual(delta.count, 58)
        XCTAssertEqual(delta.entries.map(\.index), [57, 56])
        XCTAssertEqual(delta.session?.version, 412)
    }
}
