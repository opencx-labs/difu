// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "DifuKit",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [
        .library(name: "DifuKit", targets: ["DifuKit"]),
        .executable(name: "difu-remote-probe", targets: ["difu-remote-probe"]),
    ],
    targets: [
        .target(name: "DifuKit"),
        .executableTarget(name: "difu-remote-probe", dependencies: ["DifuKit"]),
        .testTarget(name: "DifuKitTests", dependencies: ["DifuKit"]),
    ],
    swiftLanguageModes: [.v5]
)
