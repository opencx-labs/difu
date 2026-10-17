import DifuKit
import SwiftUI

/// The scrolling transcript: entries, activity line, session error and
/// pending requests. Follows new content while the user is at the bottom.
struct TranscriptView: View {
    let view: SessionView
    let model: SessionModel
    /// Bumped by the screen to force a scroll to the bottom (e.g. after sending).
    let scrollTrigger: Int
    let stop: () -> Void
    let loadFull: (EntryView) -> Void

    @State private var expanded: Set<String> = []
    /// Top of the bottom sentinel, in the scroll view's visible coordinates.
    @State private var sentinelY: CGFloat = 0
    @State private var viewportHeight: CGFloat = 0

    private var atBottom: Bool { sentinelY <= viewportHeight + 80 }

    private struct Row: Identifiable {
        let index: Int
        let entry: EntryView
        var id: String { "\(index)·\(entry.id)" }
    }

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 14) {
                    if view.entries.isEmpty, view.pending.isEmpty, !view.active {
                        emptyState
                    }
                    ForEach(rows) { row in
                        let entry = model.fullEntries[row.entry.id] ?? row.entry
                        let key = row.id
                        EntryRow(
                            entry: entry,
                            expanded: expanded.contains(key),
                            loadingFull: model.loadingEntries.contains(entry.id),
                            toggle: { toggle(key, entry: entry) },
                            loadFull: { loadFull(entry) })
                    }
                    if view.active {
                        ActivityLine(view: view, stop: stop)
                    }
                    if let error = view.error {
                        ErrorRow(text: error)
                    }
                    ForEach(view.pending) { pending in
                        PendingCard(pending: pending, sessionID: view.id)
                    }
                    Color.clear
                        .frame(height: 1)
                        .id("bottom")
                        .onGeometryChange(for: CGFloat.self) { geometry in
                            geometry.frame(in: .scrollView).minY
                        } action: { y in
                            sentinelY = y
                        }
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 12)
            }
            .defaultScrollAnchor(.bottom)
            .scrollDismissesKeyboard(.interactively)
            .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { viewportHeight = $0 }
            .onChange(of: view.version) {
                if atBottom { scrollToBottom(proxy, animated: true) }
            }
            .onChange(of: view.entries.count) {
                if atBottom { scrollToBottom(proxy, animated: true) }
            }
            .onChange(of: scrollTrigger) {
                scrollToBottom(proxy, animated: true)
            }
            .overlay(alignment: .bottomTrailing) {
                if !atBottom {
                    Button {
                        scrollToBottom(proxy, animated: true)
                    } label: {
                        Image(systemName: "arrow.down")
                            .font(.callout.weight(.semibold))
                            .padding(10)
                            .background(.regularMaterial, in: Circle())
                            .shadow(radius: 2)
                    }
                    .padding(14)
                    .transition(.scale.combined(with: .opacity))
                    .accessibilityLabel("Jump to latest")
                }
            }
            .animation(.easeOut(duration: 0.15), value: atBottom)
        }
    }

    private var rows: [Row] {
        view.entries.enumerated().map { Row(index: $0.offset, entry: $0.element) }
    }

    private var emptyState: some View {
        ContentUnavailableView(
            "No messages yet", systemImage: "text.bubble",
            description: Text("Send a message to get the agent started."))
            .padding(.top, 40)
    }

    private func toggle(_ key: String, entry: EntryView) {
        let expanding = !expanded.contains(key)
        withAnimation(.snappy(duration: 0.2)) {
            if expanding { expanded.insert(key) } else { expanded.remove(key) }
        }
        // Expanding a shortened row fetches the full one (`session.entry`).
        if expanding, entry.truncated { loadFull(entry) }
    }

    private func scrollToBottom(_ proxy: ScrollViewProxy, animated: Bool) {
        if animated {
            withAnimation(.easeOut(duration: 0.2)) { proxy.scrollTo("bottom", anchor: .bottom) }
        } else {
            proxy.scrollTo("bottom", anchor: .bottom)
        }
    }
}
