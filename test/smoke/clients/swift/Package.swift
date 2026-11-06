// swift-tools-version:5.9
import PackageDescription

// Subscribe-only smoke client built on the published Swift package. `from:`
// resolves to the latest 0.x at `swift package update` time (no Package.resolved
// is committed, so it never pins). The package pulls a prebuilt MoqFFI.xcframework.
let package = Package(
    name: "smoke",
    platforms: [.macOS(.v12)],
    dependencies: [
        .package(url: "https://github.com/moq-dev/moq-swift", from: "0.2.0"),
    ],
    targets: [
        .executableTarget(
            name: "smoke",
            dependencies: [.product(name: "Moq", package: "moq-swift")],
            path: "Sources/smoke"
        ),
    ]
)
