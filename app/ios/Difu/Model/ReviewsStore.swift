import DifuKit
import Observation
import SwiftUI

/// Cached review inbox and repository list (kept across tab switches).
@MainActor
@Observable
final class ReviewsStore {
    private(set) var inbox: Inbox?
    private(set) var inboxError: String?
    private(set) var loadingInbox = false
    private(set) var repositories: [ReviewRepository]?
    private(set) var repositoriesError: String?

    @ObservationIgnored var api: DifuAPI!

    func loadInbox(refresh: Bool) async {
        loadingInbox = true
        defer { loadingInbox = false }
        do {
            inbox = try await api.inbox(refresh: refresh)
            inboxError = nil
        } catch {
            inboxError = error.localizedDescription
        }
    }

    func loadRepositories() async {
        do {
            repositories = try await api.reviewRepositories()
            repositoriesError = nil
        } catch {
            repositoriesError = error.localizedDescription
        }
    }

    func reset() {
        inbox = nil
        inboxError = nil
        repositories = nil
        repositoriesError = nil
    }
}

/// State for one pull request screen.
@MainActor
@Observable
final class PrModel {
    let repo: String
    let number: Int

    var overview: PrOverview?
    var overviewError: String?
    var files: [DiffFile]?
    var filesError: String?
    var guide: Guide?
    var guideError: String?
    var startingGuide = false

    @ObservationIgnored private let api: DifuAPI

    init(repo: String, number: Int, api: DifuAPI) {
        self.repo = repo
        self.number = number
        self.api = api
    }

    func loadOverview() async {
        do {
            overview = try await api.overview(repo: repo, number: number)
            overviewError = nil
        } catch {
            overviewError = error.localizedDescription
        }
    }

    func loadDiff() async {
        do {
            files = try await api.diff(repo: repo, number: number)
            filesError = nil
        } catch {
            filesError = error.localizedDescription
        }
    }

    func loadGuide() async {
        do {
            guide = try await api.guide(repo: repo, number: number)
            guideError = nil
        } catch {
            guideError = error.localizedDescription
        }
    }

    func generateGuide() async {
        startingGuide = true
        defer { startingGuide = false }
        do {
            guide = try await api.generateGuide(repo: repo, number: number)
            guideError = nil
        } catch {
            guideError = error.localizedDescription
        }
    }

    /// Polls `pr.guide` every 3 s while the guide is generating.
    func pollGuide() async {
        while guide?.status == .running, !Task.isCancelled {
            try? await Task.sleep(for: .seconds(3))
            guard !Task.isCancelled else { return }
            await loadGuide()
        }
    }

    /// Runs a write that needs the current head; reloads the overview after.
    func write(_ action: (DifuAPI, String) async throws -> String) async throws -> String {
        guard let head = overview?.head, !head.isEmpty else {
            throw RemoteError.server("The pull request hasn't loaded yet.")
        }
        do {
            let message = try await action(api, head)
            await loadOverview()
            return message
        } catch {
            await loadOverview()
            if error.localizedDescription.localizedCaseInsensitiveContains("head changed") {
                throw RemoteError.server(
                    "The pull request changed since you opened it. It has been reloaded — "
                        + "review the latest changes and try again.")
            }
            throw error
        }
    }
}
