import Foundation

/// A PR image downloaded by the computer or server (`pr.image`), which has
/// the GitHub login the phone lacks.
public struct PrImage: Hashable, Sendable, Decodable {
    /// The image file: PNG, JPEG, WebP or GIF (drawn as its first frame).
    public var data: Data
    public var mime: String
    /// Pixels.
    public var width: Int
    public var height: Int

    public init(data: Data, mime: String, width: Int, height: Int) {
        self.data = data
        self.mime = mime
        self.width = width
        self.height = height
    }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        guard let encoded = f.string("data"), let data = Data(base64Encoded: encoded), !data.isEmpty else {
            throw RemoteError.protocolError("pr.image returned no image data")
        }
        self.data = data
        mime = f.string("mime") ?? "application/octet-stream"
        width = max(0, f.int("width") ?? 0)
        height = max(0, f.int("height") ?? 0)
    }

    /// Width over height, when both are known.
    public var aspectRatio: Double? {
        width > 0 && height > 0 ? Double(width) / Double(height) : nil
    }
}
