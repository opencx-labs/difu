import AppKit
import ImageIO
import UniformTypeIdentifiers

// Opaque 1024×1024 app icon: lowercase "difu" in SF Mono.
// Build and run from any directory:
//   swiftc -O app/ios/scripts/app-icon.swift -o /tmp/app-icon
//   /tmp/app-icon <out.png> [text hex, default 00FF41] [background hex, default 050805]
func color(_ hex: String) -> CGColor {
    let value = UInt32(hex.trimmingCharacters(in: CharacterSet(charactersIn: "#")), radix: 16) ?? 0
    return CGColor(srgbRed: CGFloat((value >> 16) & 0xFF) / 255, green: CGFloat((value >> 8) & 0xFF) / 255,
                   blue: CGFloat(value & 0xFF) / 255, alpha: 1)
}
let args = CommandLine.arguments
let out = args.count > 1 ? args[1] : "AppIcon.png"
let ink = color(args.count > 2 ? args[2] : "00FF41")
let paper = color(args.count > 3 ? args[3] : "050805")
let side = 1024
let space = CGColorSpace(name: CGColorSpace.sRGB)!
let context = CGContext(data: nil, width: side, height: side, bitsPerComponent: 8, bytesPerRow: 0,
                        space: space, bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)!
context.setFillColor(paper)
context.fill(CGRect(x: 0, y: 0, width: side, height: side))
let font = NSFont.monospacedSystemFont(ofSize: 300, weight: .semibold)
let text = NSAttributedString(string: "difu", attributes: [
    .font: font, .foregroundColor: NSColor(cgColor: ink)!,
])
let line = CTLineCreateWithAttributedString(text)
let bounds = CTLineGetImageBounds(line, context)
context.setShadow(offset: .zero, blur: 40, color: ink.copy(alpha: 0.55))
context.textPosition = CGPoint(x: (CGFloat(side) - bounds.width) / 2 - bounds.minX,
                               y: (CGFloat(side) - bounds.height) / 2 - bounds.minY)
CTLineDraw(line, context)
let image = context.makeImage()!
let destination = CGImageDestinationCreateWithURL(URL(fileURLWithPath: out) as CFURL,
                                                  UTType.png.identifier as CFString, 1, nil)!
CGImageDestinationAddImage(destination, image, nil)
guard CGImageDestinationFinalize(destination) else { exit(1) }
