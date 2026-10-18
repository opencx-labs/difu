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
    /// Rows drawn from the end; "Show earlier" raises it a page at a time.
    @State private var shown = TranscriptView.page
    /// The bottom sentinel is within reach of the visible area. Only this
    /// boolean is stored: writing raw scroll positions to state re-renders the
    /// transcript on every layout pass and can feed back into layout forever.
    @State private var atBottom = true

    private static let page = 120

    private struct Row: Identifiable {
        let index: Int
        let entry: EntryView
        var id: String { "\(index)·\(entry.id)" }
    }

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                // A plain VStack: a LazyVStack of these uneven rows can keep
                // re-measuring after a scroll and never settle (100% CPU).
                // Only the latest rows are drawn, so long sessions stay cheap.
                VStack(alignment: .leading, spacing: 14) {
                    if view.entries.isEmpty, view.pending.isEmpty, !view.active {
                        emptyState
                    }
                    let rows = rows
                    if rows.count > shown {
                        Button("Show \(min(Self.page, rows.count - shown)) earlier") {
                            shown += Self.page
                        }
                        .font(.callout)
                        .frame(maxWidth: .infinity)
                    }
                    ForEach(rows.suffix(shown)) { row in
                        let entry = model.fullEntries[row.entry.id] ?? row.entry
                        let key = row.id
                        EntryRow(
                            entry: entry,
                            active: view.active,
                            expanded: expanded.contains(key),
                            loadingFull: model.loadingEntries.contains(entry.id),
                            toggle: { toggle(key, entry: entry) },
                            loadFull: { loadFull(entry) }
                        )
                        .equatable()
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
                        .onGeometryChange(for: Bool.self) { geometry in
                            let viewport = geometry.bounds(of: .scrollView)?.height ?? .infinity
                            return geometry.frame(in: .scrollView).minY <= viewport + 80
                        } action: { near in
                            atBottom = near
                        }
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 12)
            }
            // No `.defaultScrollAnchor(.bottom)`: with a LazyVStack of uneven rows
            // it keeps re-anchoring as rows load and never stops laying out.
            .scrollDismissesKeyboard(.interactively)
            .onAppear { scrollToBottom(proxy, animated: false) }
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
                ZStack {
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
                // Animate only the button; animating the scroll content on this
                // change would move the sentinel again while it animates.
                .animation(.easeOut(duration: 0.15), value: atBottom)
            }
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
