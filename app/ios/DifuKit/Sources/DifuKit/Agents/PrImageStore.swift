import Foundation
import Observation

/// Images of one pull request's description and activity, fetched through its
/// computer or server once each and kept in memory while the PR is open.
@MainActor
@Observable
public final class PrImageStore {
    public enum State: Equatable, Sendable {
        case loading
        case loaded(PrImage)
        case failed(String)
    }

    public typealias Fetch = @Sendable (_ source: String, _ head: String?) async throws -> PrImage

    public let repo: String
    /// The PR's head commit, for repository-relative images; set once the
    /// overview has loaded.
    public var head: String?
    public private(set) var states: [String: State] = [:]

    @ObservationIgnored private let fetch: Fetch
    @ObservationIgnored private let limit: Int
    /// Loaded sources, oldest first, so memory stays bounded.
    @ObservationIgnored private var loadedOrder: [String] = []

    /// `limit` caps how many loaded images stay in memory.
    public init(repo: String, head: String? = nil, limit: Int = 48, fetch: @escaping Fetch) {
        self.repo = repo
        self.head = head
        self.limit = max(1, limit)
        self.fetch = fetch
    }

    public convenience init(api: DifuAPI, repo: String, number: Int) {
        self.init(repo: repo) { source, head in
            try await api.prImage(repo: repo, number: number, head: head, url: source)
        }
    }

    public func state(_ source: String) -> State? {
        states[source]
    }

    /// Starts loading `source` unless it is loading, loaded or failed.
    @discardableResult
    public func load(_ source: String) -> Task<Void, Never>? {
        guard states[source] == nil else { return nil }
        return start(source)
    }

    /// Loads a failed image again.
    @discardableResult
    public func retry(_ source: String) -> Task<Void, Never>? {
        guard case .failed = states[source] else { return nil }
        return start(source)
    }

    /// Where the image opens in a browser.
    public func browserURL(_ source: String) -> URL? {
        MarkdownImages.browserURL(source, repo: repo, head: head)
    }

    private func start(_ source: String) -> Task<Void, Never> {
        states[source] = .loading
        let fetch = fetch
        let head = head
        return Task { [weak self] in
            let state: State
            do {
                state = .loaded(try await fetch(source, head))
            } catch {
                state = .failed(error.localizedDescription)
            }
            self?.finish(source, state)
        }
    }

    private func finish(_ source: String, _ state: State) {
        states[source] = state
        guard case .loaded = state else { return }
        loadedOrder.removeAll { $0 == source }
        loadedOrder.append(source)
        while loadedOrder.count > limit {
            states[loadedOrder.removeFirst()] = nil
        }
    }
}
