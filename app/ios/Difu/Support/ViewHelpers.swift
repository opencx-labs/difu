import SwiftUI

extension View {
    /// Presents `message` in an alert while it is non-nil.
    func errorAlert(_ message: Binding<String?>, title: String = "Something went wrong") -> some View {
        alert(
            title,
            isPresented: Binding(
                get: { message.wrappedValue != nil },
                set: { if !$0 { message.wrappedValue = nil } }),
            presenting: message.wrappedValue
        ) { _ in
            Button("OK", role: .cancel) {}
        } message: { text in
            Text(text)
        }
    }
}

/// Loading / error / content switch used by remote-backed screens.
struct Loadable<Value, Content: View>: View {
    let value: Value?
    let error: String?
    var loadingText = "Loading…"
    var retry: (() async -> Void)?
    @ViewBuilder let content: (Value) -> Content

    var body: some View {
        if let value {
            content(value)
        } else if let error {
            ContentUnavailableView {
                Label("Couldn't load", systemImage: "exclamationmark.triangle")
            } description: {
                Text(error)
            } actions: {
                if let retry {
                    Button("Try Again") { Task { await retry() } }
                        .buttonStyle(.bordered)
                }
            }
        } else {
            ProgressView(loadingText)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }
}
