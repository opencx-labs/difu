import DifuKit
import SwiftUI
import UIKit

private struct PrImagesKey: EnvironmentKey {
    static let defaultValue: PrImageStore? = nil
}

extension EnvironmentValues {
    /// Loads Markdown images through the PR's computer or server; without it
    /// images show as links.
    var prImages: PrImageStore? {
        get { self[PrImagesKey.self] }
        set { self[PrImagesKey.self] = newValue }
    }
}

/// An image block: drawn at the container's width keeping its aspect ratio,
/// tap for full screen; a placeholder while loading and a row with
/// "Open in browser" when it can't be shown.
struct MarkdownImageView: View {
    let image: MarkdownImage

    @Environment(\.prImages) private var store
    @State private var fullScreen: Viewed?

    struct Viewed: Identifiable {
        let id = UUID()
        let image: UIImage
    }

    var body: some View {
        if let store, !MarkdownImages.isVector(image.source) {
            content(store)
                .task(id: image.source) { store.load(image.source) }
                .fullScreenCover(item: $fullScreen) { viewed in
                    ImageViewer(image: viewed.image, title: image.label, browserURL: store.browserURL(image.source))
                }
        } else {
            ImageLinkRow(
                image: image,
                message: MarkdownImages.isVector(image.source) ? "SVG image" : nil,
                browserURL: store?.browserURL(image.source)
                    ?? MarkdownImages.browserURL(image.source, repo: nil, head: nil))
        }
    }

    @ViewBuilder private func content(_ store: PrImageStore) -> some View {
        switch store.state(image.source) {
        case .loaded(let loaded):
            if let picture = UIImage(data: loaded.data) {
                let size = picture.size
                Image(uiImage: picture)
                    .resizable()
                    .aspectRatio(size.width / max(size.height, 1), contentMode: .fit)
                    // Small images (badges, icons) keep their own size.
                    .frame(maxWidth: CGFloat(max(loaded.width, 1)))
                    .clipShape(RoundedRectangle(cornerRadius: 6))
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
                    .onTapGesture { fullScreen = Viewed(image: picture) }
                    .accessibilityLabel(image.label)
                    .accessibilityAddTraits([.isImage, .isButton])
                    .accessibilityHint("Opens the image full screen")
            } else {
                ImageLinkRow(
                    image: image, message: "This image couldn't be decoded on the phone.",
                    browserURL: store.browserURL(image.source))
            }
        case .failed(let message):
            ImageLinkRow(
                image: image, message: message, browserURL: store.browserURL(image.source),
                retry: { store.retry(image.source) })
        case .loading, nil:
            RoundedRectangle(cornerRadius: 6)
                .fill(Color.secondary.opacity(0.1))
                .frame(maxWidth: .infinity)
                .frame(height: 140)
                .overlay {
                    VStack(spacing: 6) {
                        ProgressView()
                        Text(image.label)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                    .padding(.horizontal)
                }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel("Loading image \(image.label)")
        }
    }
}

/// An image that isn't drawn: its label, why, and a browser link.
private struct ImageLinkRow: View {
    let image: MarkdownImage
    var message: String?
    let browserURL: URL?
    var retry: (() -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Label {
                Text(message == nil ? image.label : "Can't load image: \(image.label)")
                    .lineLimit(2)
            } icon: {
                Image(systemName: message == nil ? "photo" : "photo.badge.exclamationmark")
            }
            .font(.callout)
            if let message {
                Text(message)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(3)
            }
            HStack(spacing: 16) {
                if let browserURL {
                    Link(destination: browserURL) {
                        Label("Open in browser", systemImage: "safari")
                    }
                }
                if let retry {
                    Button("Try Again", action: retry)
                }
            }
            .font(.caption.weight(.semibold))
            .buttonStyle(.borderless)
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 8))
    }
}

/// Full-screen image with pinch and double-tap zoom.
private struct ImageViewer: View {
    let image: UIImage
    let title: String
    let browserURL: URL?

    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            ZoomableImage(image: image)
                .ignoresSafeArea(edges: .bottom)
                .background(Color.black)
                .navigationTitle(title)
                .navigationBarTitleDisplayMode(.inline)
                .toolbarBackground(.visible, for: .navigationBar)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Done") { dismiss() }
                    }
                    if let browserURL {
                        ToolbarItem(placement: .primaryAction) {
                            Link(destination: browserURL) {
                                Image(systemName: "safari")
                            }
                            .accessibilityLabel("Open in browser")
                        }
                    }
                }
        }
    }
}

/// A `UIScrollView` zooming an image: pinch to zoom, double-tap to toggle.
private struct ZoomableImage: UIViewRepresentable {
    let image: UIImage

    func makeUIView(context: Context) -> UIScrollView {
        let scroll = UIScrollView()
        scroll.delegate = context.coordinator
        scroll.minimumZoomScale = 1
        scroll.maximumZoomScale = 8
        scroll.showsHorizontalScrollIndicator = false
        scroll.showsVerticalScrollIndicator = false
        scroll.backgroundColor = .black
        let view = UIImageView(image: image)
        view.contentMode = .scaleAspectFit
        view.isUserInteractionEnabled = true
        view.accessibilityIgnoresInvertColors = true
        scroll.addSubview(view)
        context.coordinator.imageView = view
        let doubleTap = UITapGestureRecognizer(
            target: context.coordinator, action: #selector(Coordinator.doubleTapped(_:)))
        doubleTap.numberOfTapsRequired = 2
        scroll.addGestureRecognizer(doubleTap)
        return scroll
    }

    func updateUIView(_ scroll: UIScrollView, context: Context) {
        context.coordinator.layout(scroll)
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    final class Coordinator: NSObject, UIScrollViewDelegate {
        weak var imageView: UIImageView?
        private var laidOutSize: CGSize = .zero

        func viewForZooming(in scrollView: UIScrollView) -> UIView? { imageView }

        func scrollViewDidZoom(_ scrollView: UIScrollView) { center(scrollView) }

        /// Fits the image to the visible area whenever that size changes.
        func layout(_ scroll: UIScrollView) {
            guard let imageView, let image = imageView.image else { return }
            DispatchQueue.main.async { [weak self, weak scroll] in
                guard let self, let scroll, scroll.bounds.size != self.laidOutSize,
                    scroll.bounds.width > 0, scroll.bounds.height > 0
                else { return }
                self.laidOutSize = scroll.bounds.size
                scroll.zoomScale = 1
                let scale = min(
                    scroll.bounds.width / max(image.size.width, 1),
                    scroll.bounds.height / max(image.size.height, 1))
                let fitted = CGSize(width: image.size.width * scale, height: image.size.height * scale)
                imageView.frame = CGRect(origin: .zero, size: fitted)
                scroll.contentSize = fitted
                self.center(scroll)
            }
        }

        private func center(_ scroll: UIScrollView) {
            guard let imageView else { return }
            let horizontal = max(0, (scroll.bounds.width - imageView.frame.width) / 2)
            let vertical = max(0, (scroll.bounds.height - imageView.frame.height) / 2)
            scroll.contentInset = UIEdgeInsets(top: vertical, left: horizontal, bottom: vertical, right: horizontal)
        }

        @objc func doubleTapped(_ gesture: UITapGestureRecognizer) {
            guard let scroll = gesture.view as? UIScrollView, let imageView else { return }
            if scroll.zoomScale > 1 {
                scroll.setZoomScale(1, animated: true)
            } else {
                let point = gesture.location(in: imageView)
                let size = CGSize(width: scroll.bounds.width / 3, height: scroll.bounds.height / 3)
                scroll.zoom(
                    to: CGRect(
                        x: point.x - size.width / 2, y: point.y - size.height / 2, width: size.width,
                        height: size.height),
                    animated: true)
            }
        }
    }
}
