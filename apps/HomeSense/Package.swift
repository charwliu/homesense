// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "HomeSense",
    platforms: [.macOS(.v14), .iOS(.v17)],
    products: [
        .library(name: "HomeSenseCore", targets: ["HomeSenseCore"]),
        .executable(name: "homesense-probe", targets: ["HomeSenseProbe"])
    ],
    targets: [
        .target(name: "HomeSenseCore"),
        .executableTarget(name: "HomeSenseProbe", dependencies: ["HomeSenseCore"]),
        .testTarget(name: "HomeSenseCoreTests", dependencies: ["HomeSenseCore"])
    ]
)
