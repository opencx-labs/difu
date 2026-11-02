import DifuKit
import SwiftUI

/// The server's disk: free space, what uses it, and cleanups.
struct ServerDiskView: View {
    let server: ServerConnection

    @State private var disk: ServerDisk?
    @State private var error: String?
    @State private var confirming: DiskCleanAction?
    @State private var cleaning: DiskCleanAction?
    @State private var cleaned: DiskCleanResult?
    @State private var actionError: String?
    /// Bumped after measuring or cleaning so polling restarts.
    @State private var changes = 0

    var body: some View {
        Loadable(value: disk, error: error, loadingText: "Loading disk space…", retry: load) { disk in
            List {
                Section {
                    DiskUsageSummary(disk: disk)
                    Button {
                        measure()
                    } label: {
                        HStack {
                            Label("Measure Again", systemImage: "arrow.clockwise")
                            if disk.measuring {
                                Spacer()
                                ProgressView()
                            }
                        }
                    }
                    .disabled(disk.measuring || cleaning != nil)
                } footer: {
                    if let path = disk.path {
                        Text("Free space is live, for the disk holding \(path). What uses it is measured in the background.")
                    } else {
                        Text("Free space is live. What uses it is measured in the background.")
                    }
                }

                Section {
                    ForEach(DiskCleanAction.allCases) { action in
                        Button {
                            confirming = action
                        } label: {
                            CleanupRow(
                                action: action, disk: disk, cleaning: cleaning == action)
                        }
                        .disabled(cleaning != nil || !disk.breakdown.canClean(action))
                    }
                } header: {
                    Text("Clean Up")
                } footer: {
                    if cleaning != nil {
                        Text("Cleaning up. This can take a few minutes; stay on this screen to see the result.")
                    } else {
                        Text("Active sessions are never touched.")
                    }
                }

                Section {
                    LabeledContent("Session worktrees", value: Formatting.bytes(disk.breakdown.sessionsTotal))
                    LabeledContent("Repositories", value: Formatting.bytes(disk.breakdown.repositories))
                    LabeledContent("Attachments", value: Formatting.bytes(disk.breakdown.attachments))
                    LabeledContent("Caches", value: Formatting.bytes(disk.breakdown.caches))
                    if disk.breakdown.sandbox.available {
                        LabeledContent("Sandbox", value: Formatting.bytes(disk.breakdown.sandbox.total))
                    }
                    LabeledContent("Other", value: Formatting.bytes(disk.breakdown.other))
                } header: {
                    Text("What's Using Space")
                } footer: {
                    Text("Other is the rest of the used space, such as the system and other programs.")
                }

                Section("Session Worktrees") {
                    if disk.breakdown.sessions.isEmpty {
                        Text(disk.measuredAt == nil ? "Not measured yet" : "No session worktrees")
                            .foregroundStyle(.secondary)
                    }
                    ForEach(disk.breakdown.sessions) { session in
                        SessionUsageRow(session: session)
                    }
                }

                Section("Sandbox") {
                    let sandbox = disk.breakdown.sandbox
                    if sandbox.available {
                        LabeledContent("Images", value: Formatting.bytes(sandbox.images))
                        LabeledContent("Containers", value: Formatting.bytes(sandbox.containers))
                        LabeledContent("Volumes", value: Formatting.bytes(sandbox.volumes))
                    } else {
                        Text("Podman isn't installed on \(server.name).")
                            .foregroundStyle(.secondary)
                    }
                }
            }
            .listStyle(.insetGrouped)
        }
        .refreshable { await load() }
        .navigationTitle("Disk")
        .navigationBarTitleDisplayMode(.inline)
        .task(id: changes) { await pollWhileMeasuring() }
        .confirmationDialog(
            confirming.map(confirmationTitle) ?? "",
            isPresented: Binding(get: { confirming != nil }, set: { if !$0 { confirming = nil } }),
            titleVisibility: .visible,
            presenting: confirming
        ) { action in
            Button(confirmationButton(action), role: .destructive) { clean(action) }
        } message: { action in
            Text(confirmationMessage(action))
        }
        .alert(
            cleaned.map { $0.freed > 0 ? "Freed \(Formatting.bytes($0.freed))" : "Cleanup Finished" } ?? "",
            isPresented: Binding(get: { cleaned != nil }, set: { if !$0 { cleaned = nil } }),
            presenting: cleaned
        ) { _ in
            Button("OK", role: .cancel) {}
        } message: { result in
            Text(result.summary.isEmpty ? (result.freed > 0 ? "Done." : "Nothing was removed.") : result.summary)
        }
        .errorAlert($actionError)
    }

    // MARK: Confirmation

    private func confirmationTitle(_ action: DiskCleanAction) -> String {
        switch action {
        case .archivedWorktrees: "Remove archived worktrees?"
        case .buildOutputs: "Clear build outputs?"
        case .sandbox: "Prune sandbox storage?"
        }
    }

    private func confirmationButton(_ action: DiskCleanAction) -> String {
        switch action {
        case .archivedWorktrees: "Remove Worktrees"
        case .buildOutputs: "Clear Build Outputs"
        case .sandbox: "Prune"
        }
    }

    /// What the cleanup removes and about how much it frees.
    private func confirmationMessage(_ action: DiskCleanAction) -> String {
        let breakdown = disk?.breakdown ?? DiskBreakdown()
        let space = Formatting.bytes(breakdown.expectedSpace(for: action))
        let sessions = breakdown.sessions(for: action)
        switch action {
        case .archivedWorktrees:
            return "Removes the worktrees of \(plural(sessions.count, "archived session")), about \(space):\n"
                + list(sessions) { "\($0.displayTitle) (\(Formatting.bytes($0.bytes)))" }
                + "\n\nTheir chats, branches and commits stay. Rebuildable build outputs in them are cleared first; worktrees with other uncommitted, untracked or ignored files are kept."
        case .buildOutputs:
            return "Deletes rebuildable build folders that Git ignores and that hold no tracked files (target, node_modules, .next, .turbo, dist, build, .venv, __pycache__, .gradle) in \(plural(sessions.count, "idle session")), about \(space):\n"
                + list(sessions) { "\($0.displayTitle) (\(Formatting.bytes($0.buildOutputs)))" }
                + "\n\nThe next build recreates them. Active sessions aren't touched."
        case .sandbox:
            return "Runs podman system prune -f and removes unused images except the sandbox image. Frees up to \(space)."
        }
    }

    /// The first few sessions, one per line.
    private func list(
        _ sessions: [SessionDiskUsage], limit: Int = 5, _ line: (SessionDiskUsage) -> String
    ) -> String {
        var lines = sessions.prefix(limit).map { "• " + line($0) }
        if sessions.count > limit { lines.append("…and \(sessions.count - limit) more") }
        return lines.joined(separator: "\n")
    }

    // MARK: Loading

    private func load() async {
        do {
            disk = try await server.api.disk()
            error = nil
        } catch is CancellationError {
            return
        } catch {
            self.error = Self.message(for: error, on: server)
        }
    }

    /// Loads, then keeps polling for a while when the breakdown is being measured.
    private func pollWhileMeasuring() async {
        await load()
        for _ in 0..<60 {
            guard !Task.isCancelled, disk?.measuring == true else { return }
            try? await Task.sleep(for: .seconds(3))
            guard !Task.isCancelled else { return }
            await load()
        }
    }

    private func measure() {
        Task {
            do {
                disk = try await server.api.disk(refresh: true)
                changes += 1
            } catch {
                actionError = Self.message(for: error, on: server)
            }
        }
    }

    private func clean(_ action: DiskCleanAction) {
        cleaning = action
        Task {
            defer {
                cleaning = nil
                changes += 1
            }
            do {
                cleaned = try await server.api.cleanDisk(action)
                Haptics.success()
            } catch {
                actionError = Self.message(for: error, on: server)
            }
        }
    }

    /// A readable error, pointing a server whose difu predates disk reporting
    /// at `difu server upgrade`.
    static func message(for error: Error, on server: ServerConnection) -> String {
        if let error = error as? RemoteError, error.isUnknownMethod {
            return "\(server.name) runs an older difu that doesn't report disk space. Upgrade it from your Mac with difu server upgrade <name>."
        }
        return error.localizedDescription
    }
}

/// `1 archived session`, `3 archived sessions`.
private func plural(_ number: Int, _ noun: String) -> String {
    "\(number) \(noun)\(number == 1 ? "" : "s")"
}

/// Free space, a used-space bar and the status, with what a low disk means.
struct DiskUsageSummary: View {
    let disk: ServerDisk

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline) {
                Text("\(Formatting.bytes(disk.free)) free of \(Formatting.bytes(disk.total))")
                    .font(.callout.weight(.medium))
                    .monospacedDigit()
                Spacer(minLength: 8)
                Pill(text: disk.status.label, color: disk.status.color)
            }
            ProgressView(value: disk.usedFraction)
                .tint(disk.status.color)
                .accessibilityLabel("Used")
                .accessibilityValue("\(Int((disk.usedFraction * 100).rounded())) percent")
            HStack {
                Text("\(Formatting.bytes(disk.used)) used")
                    .monospacedDigit()
                Spacer(minLength: 8)
                if disk.measuring {
                    HStack(spacing: 4) {
                        ProgressView().controlSize(.mini)
                        Text("Measuring…")
                    }
                } else if let measured = disk.measuredAt {
                    Text("Measured \(Formatting.relative(Date(milliseconds: measured)))")
                } else {
                    Text("Not measured yet")
                }
            }
            .font(.caption)
            .foregroundStyle(.secondary)
            if let warning = disk.status.warning {
                Label(warning, systemImage: "exclamationmark.triangle.fill")
                    .font(.caption)
                    .foregroundStyle(disk.status.color)
            }
        }
        .padding(.vertical, 4)
    }
}

/// One cleanup: what it works on and about how much it frees.
private struct CleanupRow: View {
    let action: DiskCleanAction
    let disk: ServerDisk
    let cleaning: Bool

    var body: some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Label(action.title, systemImage: icon)
                Text(cleaning ? "Cleaning up…" : detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
            if cleaning {
                Spacer()
                ProgressView()
            }
        }
    }

    private var icon: String {
        switch action {
        case .archivedWorktrees: "archivebox"
        case .buildOutputs: "hammer"
        case .sandbox: "cube"
        }
    }

    private var detail: String {
        let breakdown = disk.breakdown
        if action == .sandbox, !breakdown.sandbox.available {
            return "Podman isn't installed"
        }
        if disk.measuredAt == nil {
            return "Waiting for the first measurement"
        }
        guard breakdown.canClean(action) else { return "Nothing to remove" }
        let space = Formatting.bytes(breakdown.expectedSpace(for: action))
        switch action {
        case .archivedWorktrees:
            return "\(plural(breakdown.sessions(for: action).count, "archived session")) · about \(space)"
        case .buildOutputs:
            return "\(plural(breakdown.sessions(for: action).count, "idle session")) · about \(space)"
        case .sandbox:
            return "Up to \(space)"
        }
    }
}

/// A session's worktree size, with archived and active badges.
private struct SessionUsageRow: View {
    let session: SessionDiskUsage

    var body: some View {
        HStack(spacing: 8) {
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(session.displayTitle).lineLimit(1)
                    if session.active {
                        Pill(text: "Active", color: .accentColor)
                    }
                    if session.archived {
                        Pill(text: "Archived")
                    }
                }
                if session.buildOutputs > 0 {
                    Text("\(Formatting.bytes(session.buildOutputs)) of build outputs")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            Spacer(minLength: 8)
            Text(Formatting.bytes(session.bytes))
                .font(.monoCaption)
                .foregroundStyle(.secondary)
        }
        .padding(.vertical, 2)
    }
}

extension DiskStatus {
    fileprivate var color: Color {
        switch self {
        case .ok: .success
        case .low: .orange
        case .critical: .red
        case .unknown: .secondary
        }
    }

    fileprivate var warning: String? {
        switch self {
        case .low: "Running low. Below 5% or 5 GB free, new sessions, moves and clones pause until space is freed."
        case .critical: "New sessions, moves and clones are paused until space is freed. Running sessions continue."
        case .ok, .unknown: nil
        }
    }
}
