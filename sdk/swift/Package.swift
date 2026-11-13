// swift-tools-version:5.9
import PackageDescription

// Complete strict-concurrency checking keeps the Sendable story honest on every build.
let strict: [SwiftSetting] = [.enableExperimentalFeature("StrictConcurrency")]

let package = Package(
    name: "ForgeBase",
    platforms: [
        .iOS(.v15),
        .macOS(.v12),
        .tvOS(.v15),
        .watchOS(.v8),
        .visionOS(.v1),
    ],
    products: [
        .library(name: "ForgeBase", targets: ["ForgeBase"]),
    ],
    targets: [
        .target(name: "ForgeBase", swiftSettings: strict),
        .executableTarget(
            name: "ForgeBaseExample",
            dependencies: ["ForgeBase"],
            path: "Examples/ForgeBaseExample",
            swiftSettings: strict
        ),
        .testTarget(name: "ForgeBaseTests", dependencies: ["ForgeBase"], swiftSettings: strict),
    ]
)
