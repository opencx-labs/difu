import XCTest

@testable import DifuKit

final class PatchTests: XCTestCase {
    func testClassifiesHunkLines() {
        let patch = "@@ -10,4 +10,5 @@ fn main() {\n context\n-old\n+new\n+added\n\\ No newline at end of file\n"
        let lines = Patch.lines(patch)
        XCTAssertEqual(lines.map(\.kind), [.hunk, .context, .deletion, .addition, .addition, .meta])
        XCTAssertEqual(lines[1].oldNumber, 10)
        XCTAssertEqual(lines[1].newNumber, 10)
        XCTAssertEqual(lines[2].oldNumber, 11)
        XCTAssertNil(lines[2].newNumber)
        XCTAssertEqual(lines[3].newNumber, 11)
        XCTAssertEqual(lines[4].newNumber, 12)
        XCTAssertEqual(lines[3].content, "new")
        XCTAssertEqual(lines[0].content, "@@ -10,4 +10,5 @@ fn main() {")
    }

    func testFileHeadersBeforeFirstHunkAreMeta() {
        let patch = """
            diff --git a/x b/x
            index 1..2 100644
            --- a/x
            +++ b/x
            @@ -1 +1 @@
            --- not a header
            +++ also a change
            """
        XCTAssertEqual(
            Patch.lines(patch).map(\.kind),
            [.meta, .meta, .meta, .meta, .hunk, .deletion, .addition])
    }

    func testMultipleHunksResetNumbers() {
        let lines = Patch.lines("@@ -1,1 +1,1 @@\n-a\n+b\n@@ -40 +41,2 @@\n c\n+d")
        XCTAssertEqual(lines[4].oldNumber, 40)
        XCTAssertEqual(lines[4].newNumber, 41)
        XCTAssertEqual(lines[5].newNumber, 42)
    }

    func testEmptyLineInsideHunkIsContextAndCRLF() {
        let lines = Patch.lines("@@ -1,2 +1,2 @@\r\n a\r\n\r\n+b\r\n")
        XCTAssertEqual(lines.map(\.kind), [.hunk, .context, .context, .addition])
        XCTAssertEqual(lines[3].text, "+b")
    }

    func testDiffWithoutHunkHeader() {
        let lines = Patch.lines("+first\n+second")
        XCTAssertEqual(lines.map(\.kind), [.addition, .addition])
        XCTAssertEqual(lines.map(\.newNumber), [1, 2])
    }

    func testHunkHeaderParsing() {
        XCTAssertEqual(Patch.hunkStart("@@ -12,7 +14,9 @@ section")?.old, 12)
        XCTAssertEqual(Patch.hunkStart("@@ -12,7 +14,9 @@ section")?.new, 14)
        XCTAssertEqual(Patch.hunkStart("@@ -0,0 +1 @@")?.new, 1)
        XCTAssertNil(Patch.hunkStart("@@ garbage"))
    }

    func testStats() {
        let stats = Patch.stats("--- a\n+++ b\n@@ -1,2 +1,3 @@\n-x\n+y\n+z\n w")
        XCTAssertEqual(stats.additions, 2)
        XCTAssertEqual(stats.deletions, 1)
    }
}

final class MarkdownTests: XCTestCase {
    func testBlocks() {
        let blocks = Markdown.blocks(
            """
            # Title
            Some **bold** text
            continues here.

            - one
            - [x] done
              more about done
            1. first
            ```swift
            let x = 1
            ```
            > quoted
            ---
            | a | b |
            |---|---|
            | 1 | 2 |
            """)
        XCTAssertEqual(blocks.count, 7)
        XCTAssertEqual(blocks[0], .heading(level: 1, text: "Title"))
        XCTAssertEqual(blocks[1], .paragraph("Some **bold** text\ncontinues here."))
        guard case .list(let items) = blocks[2] else { return XCTFail("list") }
        XCTAssertEqual(items.map(\.marker), ["•", "", "1."])
        XCTAssertEqual(items[1].checked, true)
        XCTAssertEqual(items[1].text, "done\nmore about done")
        XCTAssertEqual(blocks[3], .code(language: "swift", code: "let x = 1"))
        XCTAssertEqual(blocks[4], .quote("quoted"))
        XCTAssertEqual(blocks[5], .rule)
        guard case .table(let table) = blocks[6] else { return XCTFail("table") }
        XCTAssertTrue(table.hasPrefix("| a | b |"))
    }

    func testUnclosedFenceAndNestedList() {
        let blocks = Markdown.blocks("text\n```\ncode\nmore")
        XCTAssertEqual(blocks, [.paragraph("text"), .code(language: nil, code: "code\nmore")])

        guard case .list(let items) = Markdown.blocks("- a\n  - b\n    - c").first else {
            return XCTFail("list")
        }
        XCTAssertEqual(items.map(\.depth), [0, 1, 2])
    }

    func testHashtagIsNotHeading() {
        XCTAssertEqual(Markdown.blocks("#hashtag"), [.paragraph("#hashtag")])
    }
}

final class FormattingTests: XCTestCase {
    func testElapsed() {
        XCTAssertEqual(Formatting.elapsed(seconds: 12), "12s")
        XCTAssertEqual(Formatting.elapsed(seconds: 65), "1m 5s")
        XCTAssertEqual(Formatting.elapsed(seconds: 7300), "2h 1m")
        XCTAssertEqual(Formatting.elapsed(seconds: -3), "0s")
        let now = Date(milliseconds: 1_790_000_012_000)
        XCTAssertEqual(Formatting.elapsed(since: 1_790_000_000_000, now: now), "12s")
    }

    func testDuration() {
        XCTAssertEqual(Formatting.duration(milliseconds: 1234), "1.2s")
        XCTAssertEqual(Formatting.duration(milliseconds: 125_000), "2m 5s")
    }

    func testRelative() {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        XCTAssertEqual(Formatting.relative(now.addingTimeInterval(-10), now: now), "now")
        XCTAssertEqual(Formatting.relative(now.addingTimeInterval(-300), now: now), "5m")
        XCTAssertEqual(Formatting.relative(now.addingTimeInterval(-7200), now: now), "2h")
        XCTAssertEqual(Formatting.relative(now.addingTimeInterval(-3 * 86_400), now: now), "3d")
    }
}
