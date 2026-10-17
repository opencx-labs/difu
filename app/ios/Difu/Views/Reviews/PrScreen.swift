import DifuKit
import SwiftUI

/// One pull request: Overview, Diff and Guide, plus review actions.
struct PrScreen: View {
    enum Tab: String, CaseIterable {
        case overview = "Overview"
        case diff = "Diff"
        case guide = "Guide"
    }

    let route: PrRoute

    @Environment(AppModel.self) private var app
    @State private var model: PrModel?
    @State private var tab = Tab.overview

    var body: some View {
        Group {
            if let model {
                VStack(spacing: 0) {
                    switch tab {
                    case .overview: PrOverviewTab(model: model)
                    case .diff: PrDiffTab(model: model)
                    case .guide: PrGuideTab(model: model)
                    }
                }
                .safeAreaInset(edge: .top, spacing: 0) {
                    Picker("Section", selection: $tab) {
                        ForEach(Tab.allCases, id: \.self) { Text($0.rawValue).tag($0) }
                    }
                    .pickerStyle(.segmented)
                    .padding(.horizontal)
                    .padding(.vertical, 8)
                    .background(.bar)
                }
                .toolbar {
                    ToolbarItem(placement: .primaryAction) { PrActionsMenu(model: model) }
                }
            } else {
                ProgressView()
            }
        }
        .navigationTitle("\(route.repo.split(separator: "/").last.map(String.init) ?? route.repo) #\(route.number)")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear {
            if model == nil { model = PrModel(repo: route.repo, number: route.number, api: app.api) }
        }
    }
}

// MARK: Overview

struct PrOverviewTab: View {
    let model: PrModel

    var body: some View {
        Loadable(value: model.overview, error: model.overviewError, loadingText: "Loading pull request…",
                 retry: model.loadOverview) { pr in
            List {
                Section {
                    VStack(alignment: .leading, spacing: 8) {
                        Text(pr.title).font(.title3.weight(.semibold))
                        HStack(spacing: 6) {
                            PrStateBadge(state: pr.state, draft: pr.draft)
                            if let author = pr.author {
                                Text("by \(author)").font(.subheadline).foregroundStyle(.secondary)
                            }
                        }
                        if let head = pr.headBranch, let base = pr.baseBranch {
                            HStack(spacing: 4) {
                                Text(head)
                                Image(systemName: "arrow.right").imageScale(.small)
                                Text(base)
                            }
                            .font(.monoCaption)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                        }
                        HStack(spacing: 8) {
                            DiffStat(additions: pr.additions, deletions: pr.deletions)
                            Text("\(pr.changedFiles) file\(pr.changedFiles == 1 ? "" : "s")")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                    .padding(.vertical, 4)
                    if pr.isOpen {
                        MergeabilityRow(mergeable: pr.mergeable, mergeState: pr.mergeState)
                    }
                    if let url = pr.url {
                        Link(destination: url) {
                            Label("Open on GitHub", systemImage: "safari")
                        }
                    }
                }

                if !pr.reviewers.isEmpty {
                    Section("Reviewers") {
                        ForEach(pr.reviewers) { reviewer in
                            ReviewerRow(reviewer: reviewer)
                        }
                    }
                }

                if !pr.checks.isEmpty {
                    Section {
                        ForEach(pr.checks) { check in
                            CheckRow(check: check)
                        }
                    } header: {
                        let failing = pr.checks.filter { $0.state == .fail }.count
                        let pending = pr.checks.filter { $0.state == .pending || $0.state == .expected }.count
                        Text("Checks" + (failing > 0 ? " · \(failing) failing" : pending > 0 ? " · \(pending) pending" : ""))
                    }
                }

                Section("Description") {
                    if pr.body.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                        Text("No description provided.").foregroundStyle(.secondary)
                    } else {
                        MarkdownText(source: pr.body, font: .callout)
                            .textSelection(.enabled)
                            .padding(.vertical, 4)
                    }
                }

                if !pr.timeline.isEmpty {
                    Section("Timeline") {
                        ForEach(Array(pr.timeline.enumerated()), id: \.offset) { _, event in
                            TimelineRow(event: event)
                        }
                    }
                }
            }
            .listStyle(.insetGrouped)
            .refreshable { await model.loadOverview() }
        }
        .task {
            if model.overview == nil { await model.loadOverview() }
        }
    }
}

struct PrStateBadge: View {
    let state: String
    let draft: Bool

    var body: some View {
        let (label, icon, color) = style
        Pill(text: label, systemImage: icon, color: color)
    }

    private var style: (String, String, Color) {
        switch state.lowercased() {
        case "merged": ("Merged", "arrow.triangle.merge", .purple)
        case "closed": ("Closed", "xmark.circle", .red)
        default: draft ? ("Draft", "pencil.circle", .secondary) : ("Open", "arrow.triangle.pull", .green)
        }
    }
}

private struct MergeabilityRow: View {
    let mergeable: String?
    let mergeState: String?

    var body: some View {
        let (text, icon, color) = describe
        Label {
            VStack(alignment: .leading, spacing: 1) {
                Text(text)
                if let mergeState, !mergeState.isEmpty {
                    Text(mergeState.replacingOccurrences(of: "_", with: " ").capitalized)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        } icon: {
            Image(systemName: icon).foregroundStyle(color)
        }
    }

    private var describe: (String, String, Color) {
        switch (mergeable ?? "").uppercased() {
        case "CONFLICTING": return ("Has conflicts", "exclamationmark.triangle.fill", .red)
        case "MERGEABLE":
            switch (mergeState ?? "").uppercased() {
            case "BLOCKED": return ("Blocked by branch protection", "lock.fill", .orange)
            case "BEHIND": return ("Behind the base branch", "arrow.down.circle", .orange)
            case "UNSTABLE": return ("Mergeable, some checks failing", "exclamationmark.circle", .orange)
            case "DRAFT": return ("Draft — mark ready to merge", "pencil.circle", .secondary)
            default: return ("Ready to merge", "checkmark.circle.fill", .green)
            }
        default: return ("Checking mergeability…", "clock", .secondary)
        }
    }
}

private struct ReviewerRow: View {
    let reviewer: PrReviewer

    var body: some View {
        let (icon, color, label) = style
        HStack {
            Image(systemName: icon).foregroundStyle(color).frame(width: 20)
            Text(reviewer.name)
            Spacer()
            Text(label).font(.caption).foregroundStyle(.secondary)
        }
    }

    private var style: (String, Color, String) {
        let state = reviewer.state.lowercased()
        if state.contains("approved") { return ("checkmark.circle.fill", .green, "Approved") }
        if state.contains("changes") { return ("exclamationmark.circle.fill", .red, "Changes requested") }
        if state.contains("comment") { return ("text.bubble", .secondary, "Commented") }
        if state.contains("dismiss") { return ("minus.circle", .secondary, "Dismissed") }
        return ("clock", .orange, "Pending")
    }
}

private struct CheckRow: View {
    let check: PrCheck

    var body: some View {
        let (icon, color) = style
        let row = HStack(spacing: 8) {
            Image(systemName: icon).foregroundStyle(color).frame(width: 20)
            Text(check.name).lineLimit(2)
            Spacer(minLength: 4)
            if check.url != nil {
                Image(systemName: "arrow.up.right.square").foregroundStyle(.tertiary)
            }
        }
        if let url = check.url {
            Link(destination: url) { row }
                .foregroundStyle(.primary)
        } else {
            row
        }
    }

    private var style: (String, Color) {
        switch check.state {
        case .pass: ("checkmark.circle.fill", .green)
        case .fail: ("xmark.circle.fill", .red)
        case .pending, .expected: ("clock.fill", .orange)
        case .skipping: ("forward.circle", .secondary)
        case .cancelled: ("slash.circle", .secondary)
        case .unknown: ("questionmark.circle", .secondary)
        }
    }
}

private struct TimelineRow: View {
    let event: TimelineEvent

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 4) {
                Text(event.author ?? "someone").fontWeight(.semibold)
                Text(event.kind).foregroundStyle(.secondary)
                Spacer(minLength: 4)
                if let date = event.date {
                    Text(Formatting.relative(date)).foregroundStyle(.secondary)
                }
                if let url = event.url {
                    Link(destination: url) { Image(systemName: "arrow.up.right.square") }
                        .buttonStyle(.borderless)
                }
            }
            .font(.caption)
            if !event.body.isEmpty {
                MarkdownText(source: event.body, font: .callout)
                    .lineLimit(12)
            }
        }
        .padding(.vertical, 2)
    }
}

// MARK: Diff

struct PrDiffTab: View {
    let model: PrModel

    var body: some View {
        Loadable(value: model.files, error: model.filesError, loadingText: "Loading diff…", retry: model.loadDiff) {
            files in
            DiffFileList(files: files)
                .refreshable { await model.loadDiff() }
        }
        .task {
            if model.files == nil { await model.loadDiff() }
        }
    }
}
