import XCTest

@testable import DifuKit

final class MarkdownImageTests: XCTestCase {
    func testMarkdownAndHTMLImagesBecomeTheirOwnBlocks() {
        let source = """
            Before ![Screen **one**](docs/one.png "Title") after.

            [![CI](https://example.com/ci.svg)](https://example.com/runs)

            <p align="center">
              <img width="600" alt="Login &amp; signup" src="https://github.com/user-attachments/assets/abc?x=1&amp;y=2">
            </p>

            ```md
            ![example](fake.png)
            ```
            Inline `![code](fake.png)` stays text.
            """
        XCTAssertEqual(
            Markdown.blocks(source),
            [
                .paragraph("Before"),
                .image(MarkdownImage(source: "docs/one.png", alt: "Screen **one**")),
                .paragraph("after."),
                .image(MarkdownImage(source: "https://example.com/ci.svg", alt: "CI", link: "https://example.com/runs")),
                .image(
                    MarkdownImage(
                        source: "https://github.com/user-attachments/assets/abc?x=1&y=2", alt: "Login & signup")),
                .code(language: "md", code: "![example](fake.png)"),
                .paragraph("Inline `![code](fake.png)` stays text."),
            ])
    }

    func testOnlyRealImageSyntaxIsSplitOut() {
        XCTAssertEqual(MarkdownImages.split("A [link](https://a.example) and ! bang"), [.text("A [link](https://a.example) and ! bang")])
        XCTAssertEqual(MarkdownImages.split("<imgx src='a.png'> <b>bold</b>"), [.text("<imgx src='a.png'> <b>bold</b>")])
        XCTAssertEqual(
            MarkdownImages.split("![](<docs/with space.png>)"),
            [.image(MarkdownImage(source: "docs/with space.png"))])
        XCTAssertEqual(
            MarkdownImages.split("<IMG SRC=a.png ALT=x />"), [.image(MarkdownImage(source: "a.png", alt: "x"))])
        XCTAssertEqual(MarkdownImages.split("<img alt='no source'>"), [], "an img without src is dropped")
        XCTAssertEqual(MarkdownImages.split("![broken](no-close"), [.text("![broken](no-close")])
        XCTAssertEqual(MarkdownImage(source: "a.png", alt: "  ").label, "Image")
    }

    func testBrowserLinksFollowTheTerminalsRules() {
        XCTAssertEqual(
            MarkdownImages.browserURL("https://example.com/a.png", repo: "o/r", head: "abc")?.absoluteString,
            "https://example.com/a.png")
        XCTAssertEqual(
            MarkdownImages.browserURL("/o/r/raw/main/a.png", repo: nil, head: nil)?.absoluteString,
            "https://github.com/o/r/raw/main/a.png")
        XCTAssertEqual(
            MarkdownImages.browserURL("./docs/a.png", repo: "o/r", head: "abc")?.absoluteString,
            "https://github.com/o/r/blob/abc/docs/a.png")
        XCTAssertNil(MarkdownImages.browserURL("docs/a.png", repo: "o/r", head: nil))
        XCTAssertNil(MarkdownImages.browserURL("javascript:alert(1)", repo: "o/r", head: "abc"))
        XCTAssertNil(MarkdownImages.browserURL("file:///etc/passwd", repo: "o/r", head: "abc"))
        XCTAssertTrue(MarkdownImages.isVector("https://img.shields.io/badge.SVG?style=flat"))
        XCTAssertFalse(MarkdownImages.isVector("https://example.com/a.png#svg"))
    }

    func testPrImagesDecodeAndTheAPIAsksTheServer() async throws {
        let image = try decode(PrImage.self, #"{"data": "AQID", "mime": "image/png", "width": 640, "height": 480}"#)
        XCTAssertEqual(image.data, Data([1, 2, 3]))
        XCTAssertEqual(image.aspectRatio ?? 0, 4.0 / 3.0, accuracy: 0.001)
        XCTAssertThrowsError(try decode(PrImage.self, #"{"data": "%%%", "mime": "image/png"}"#))

        let recorder = ImageCalls()
        let api = DifuAPI(call: recorder.call)
        let fetched = try await api.prImage(repo: "o/r", number: 7, head: "abc", url: "docs/a.png")
        XCTAssertEqual(fetched.width, 1)
        let params = await recorder.params
        XCTAssertEqual(params["repo"], .string("o/r"))
        XCTAssertEqual(params["head"], .string("abc"))
        XCTAssertEqual(params["url"], .string("docs/a.png"))
    }
}

private actor ImageCalls {
    private(set) var params: [String: JSONValue] = [:]

    nonisolated var call: DifuAPI.Call {
        { method, params in await self.record(method, params) }
    }

    private func record(_ method: String, _ params: [String: JSONValue]) -> Data {
        self.params = params
        self.params["method"] = .string(method)
        return Data(#"{"id": 1, "result": {"data": "AQID", "mime": "image/png", "width": 1, "height": 1}}"#.utf8)
    }
}

/// Counts fetches and fails sources named `bad`.
private actor Fetches {
    private(set) var sources: [String] = []
    private(set) var heads: [String?] = []

    func fetch(_ source: String, _ head: String?) throws -> PrImage {
        sources.append(source)
        heads.append(head)
        if source.contains("bad") { throw RemoteError.server("Image download returned HTTP 404") }
        return PrImage(data: Data([1]), mime: "image/png", width: 10, height: 5)
    }
}

@MainActor
final class PrImageStoreTests: XCTestCase {
    func testImagesLoadOnceAndFailuresCanBeRetried() async throws {
        let fetches = Fetches()
        let store = PrImageStore(repo: "o/r", head: "abc") { source, head in
            try await fetches.fetch(source, head)
        }
        let first = store.load("a.png")
        XCTAssertEqual(store.state("a.png"), .loading)
        XCTAssertNil(store.load("a.png"), "already loading")
        await first?.value
        XCTAssertEqual(store.state("a.png"), .loaded(PrImage(data: Data([1]), mime: "image/png", width: 10, height: 5)))
        XCTAssertNil(store.load("a.png"), "cached in memory")
        XCTAssertNil(store.retry("a.png"), "only failures retry")

        await store.load("bad.png")?.value
        XCTAssertEqual(store.state("bad.png"), .failed("Image download returned HTTP 404"))
        XCTAssertNil(store.load("bad.png"), "a failure stays until retried")
        await store.retry("bad.png")?.value
        let sources = await fetches.sources
        XCTAssertEqual(sources, ["a.png", "bad.png", "bad.png"])
        let heads = await fetches.heads
        XCTAssertEqual(heads, ["abc", "abc", "abc"])
        XCTAssertEqual(store.browserURL("docs/x.png")?.absoluteString, "https://github.com/o/r/blob/abc/docs/x.png")
    }

    func testOnlyTheNewestImagesStayInMemory() async {
        let fetches = Fetches()
        let store = PrImageStore(repo: "o/r", limit: 2) { source, head in try await fetches.fetch(source, head) }
        for source in ["1.png", "2.png", "3.png"] {
            await store.load(source)?.value
        }
        XCTAssertNil(store.state("1.png"))
        XCTAssertNotNil(store.state("2.png"))
        XCTAssertNotNil(store.state("3.png"))
    }
}
