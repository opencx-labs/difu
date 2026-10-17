import DifuKit
import Observation
import SwiftUI

/// The agents list plus every open (watched) session.
@MainActor
@Observable
final class SessionStore {
    private(set) var summaries: [SessionSummary] = []
    private(set) var loaded = false
    private(set) var error: String?

    @ObservationIgnored var api: DifuAPI!
    @ObservationIgnored private var models: [String: SessionModel] = [:]
    /// Open screens per session id; watched while > 0.
    @ObservationIgnored private var watchers: [String: Int] = [:]

    func summary(for id: String) -> SessionSummary? {
        summaries.first { $0.id == id }
    }

    /// The live state for a session screen (created on demand).
    func model(for id: String) -> SessionModel {
        if let model = models[id] { return model }
        let model = SessionModel(id: id)
        models[id] = model
        return model
    }

    // MARK: Connection

    /// Subscribes to the list and re-watches open sessions after (re)connecting.
    func connected() async {
        do {
            receive(try await api.subscribeSessions())
        } catch {
            self.error = error.localizedDescription
        }
        for id in watchers.keys {
            await watch(id)
        }
    }

    func refresh() async {
        do {
            receive(try await api.listSessions())
        } catch {
            self.error = error.localizedDescription
        }
    }

    func reset() {
        summaries = []
        loaded = false
        error = nil
        models = [:]
        watchers = [:]
    }

    // MARK: Watching

    func open(_ id: String) {
        watchers[id, default: 0] += 1
        if watchers[id] == 1 {
            Task { await watch(id) }
        }
    }

    func close(_ id: String) {
        guard let count = watchers[id] else { return }
        if count > 1 {
            watchers[id] = count - 1
            return
        }
        watchers[id] = nil
        Task { try? await api.unwatch(id) }
    }

    func watch(_ id: String) async {
        let model = model(for: id)
        do {
            try await api.watch(id)
            model.watchError = nil
        } catch {
            if model.view == nil { model.watchError = error.localizedDescription }
        }
    }

    // MARK: Events

    func receive(_ list: [SessionSummary]) {
        summaries = list
        loaded = true
        error = nil
    }

    func receive(_ view: SessionView) {
        let model = model(for: view.id)
        model.view = view
        model.closedMessage = nil
        model.watchError = nil
    }

    func apply(_ delta: SessionDelta) {
        guard watchers[delta.id] != nil else { return }
        guard let model = models[delta.id], var view = model.view else {
            Task { await watch(delta.id) }
            return
        }
        model.view = nil  // keep the transcript uniquely referenced while patching
        let outcome = view.apply(delta)
        model.view = view
        if outcome == .needsRewatch {
            Task { await watch(delta.id) }
        }
    }

    func closed(_ id: String, message: String?) {
        guard let model = models[id] else { return }
        model.closedMessage = message ?? "This session is no longer available."
    }
}

/// One session's live transcript and per-screen caches.
@MainActor
@Observable
final class SessionModel: Identifiable {
    let id: String
    var view: SessionView?
    var closedMessage: String?
    var watchError: String?
    /// Untruncated rows fetched with `session.entry`, by entry id.
    var fullEntries: [String: EntryView] = [:]
    var loadingEntries: Set<String> = []

    init(id: String) {
        self.id = id
    }
}
