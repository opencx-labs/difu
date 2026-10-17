import PhotosUI
import SwiftUI
import UIKit

/// A photo chosen for a message, re-encoded as JPEG.
struct PickedImage: Identifiable {
    let id = UUID()
    let thumbnail: UIImage
    let jpeg: Data

    /// Longest edge sent to the agent.
    static let maxDimension: CGFloat = 2048

    /// Loads a picker item, downscales it and encodes JPEG off the main thread.
    static func load(_ item: PhotosPickerItem) async -> PickedImage? {
        guard let data = try? await item.loadTransferable(type: Data.self) else { return nil }
        return await Task.detached(priority: .userInitiated) { encode(data) }.value
    }

    nonisolated static func encode(_ data: Data) -> PickedImage? {
        guard let image = UIImage(data: data) else { return nil }
        let size = image.size
        let scale = min(1, maxDimension / max(size.width, size.height, 1))
        let target = CGSize(width: floor(size.width * scale), height: floor(size.height * scale))
        let format = UIGraphicsImageRendererFormat.default()
        format.scale = 1
        let resized = UIGraphicsImageRenderer(size: target, format: format).image { _ in
            image.draw(in: CGRect(origin: .zero, size: target))
        }
        guard let jpeg = resized.jpegData(compressionQuality: 0.82) else { return nil }
        let thumbnail = resized.preparingThumbnail(of: CGSize(width: 160, height: 160)) ?? resized
        return PickedImage(thumbnail: thumbnail, jpeg: jpeg)
    }
}
