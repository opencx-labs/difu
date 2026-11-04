import DifuKit
import SwiftUI
import WebKit

/// The session's presented HTML artifacts; each opens in a web view.
struct ArtifactsSheet: View {
    let sessionID: String
    let artifacts: [SessionArtifact]

    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List(artifacts) { artifact in
                NavigationLink {
                    ArtifactViewer(sessionID: sessionID, artifact: artifact)
                } label: {
                    VStack(alignment: .leading, spacing: 3) {
                        Label(artifact.title, systemImage: "doc.richtext")
                            .font(.body.weight(.medium))
                        Text(artifact.path)
                            .font(.monoCaption)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                }
            }
            .overlay {
                if artifacts.isEmpty {
                    ContentUnavailableView(
                        "No artifacts", systemImage: "doc.richtext",
                        description: Text("HTML reports the agent presents appear here."))
                }
            }
            .navigationTitle("Artifacts")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
    }
}

/// One artifact, read from the computer or server running the session. Like
/// the terminal's preview, it doesn't reload by itself: **Refresh** reads it
/// again. Links open in the browser; the page can't reach the session's files.
struct ArtifactViewer: View {
    let sessionID: String
    let artifact: SessionArtifact

    @Environment(ServerConnection.self) private var server
    @State private var file: ArtifactFile?
    @State private var error: String?
    @State private var loading = false
    @State private var shared: URL?

    var body: some View {
        Loadable(value: file, error: error, loadingText: "Loading \(artifact.name)…", retry: load) { file in
            ArtifactWebView(html: file.html)
                .ignoresSafeArea(edges: .bottom)
        }
        .navigationTitle(artifact.title)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                if let shared {
                    ShareLink(item: shared) {
                        Label("Share", systemImage: "square.and.arrow.up")
                    }
                }
                Button {
                    Task { await load() }
                } label: {
                    if loading {
                        ProgressView()
                    } else {
                        Label("Refresh", systemImage: "arrow.clockwise")
                    }
                }
                .disabled(loading)
            }
        }
        .task { await load() }
    }

    private func load() async {
        loading = true
        defer { loading = false }
        do {
            let file = try await server.api.artifact(sessionID, artifact: artifact)
            self.file = file
            error = nil
            shared = save(file)
        } catch let remote as RemoteError where remote.isUnknownMethod {
            error = "Update difu on \(server.name) to open artifacts from the phone."
        } catch {
            self.error = error.localizedDescription
        }
    }

    /// A copy to share or open in another app (Safari, Files).
    private func save(_ file: ArtifactFile) -> URL? {
        let directory = FileManager.default.temporaryDirectory.appending(path: "artifacts", directoryHint: .isDirectory)
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let url = directory.appending(path: file.name)
        return (try? file.data.write(to: url, options: .atomic)) == nil ? nil : url
    }
}

/// Self-contained HTML in a web view with no base URL: scripts run, but the
/// page can't read files, and links leave for the browser.
struct ArtifactWebView: UIViewRepresentable {
    let html: String

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeUIView(context: Context) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        let view = WKWebView(frame: .zero, configuration: configuration)
        view.navigationDelegate = context.coordinator
        view.allowsBackForwardNavigationGestures = false
        view.isInspectable = false
        return view
    }

    func updateUIView(_ view: WKWebView, context: Context) {
        guard context.coordinator.loaded != html else { return }
        context.coordinator.loaded = html
        view.loadHTMLString(html, baseURL: nil)
    }

    final class Coordinator: NSObject, WKNavigationDelegate {
        var loaded: String?

        @MainActor
        func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction) async
            -> WKNavigationActionPolicy
        {
            // The page itself and in-page anchors (`about:blank#…`) stay;
            // clicked web links open outside, anything else goes nowhere.
            guard action.navigationType == .linkActivated, let url = action.request.url else {
                return .allow
            }
            let scheme = url.scheme?.lowercased() ?? ""
            if scheme == "about" { return .allow }
            if ["http", "https", "mailto"].contains(scheme) {
                UIApplication.shared.open(url, options: [:], completionHandler: nil)
            }
            return .cancel
        }
    }
}
