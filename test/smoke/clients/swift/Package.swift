// swift-tools-version:5.9
import Foundation
import PackageDescription

// Subscribe-only smoke client built on the published Swift package, which pulls
// a prebuilt MoqFFI.xcframework.
//
// Latest mode (default): `from:` resolves to the latest 0.x at `swift package
// update` time (no Package.resolved is committed, so it never pins). Pinned
// mode: a release sets MOQ_SWIFT_VERSION to the exact version it just cut, so
// the smoke run tests that build rather than whatever happens to be newest.
let moqURL = "https://github.com/moq-dev/moq-swift"
let moqPin = ProcessInfo.processInfo.environment["MOQ_SWIFT_VERSION"]
    .flatMap { $0.isEmpty ? nil : Version($0) }
let moqDependency: Package.Dependency =
    moqPin.map { .package(url: moqURL, exact: $0) }
    ?? .package(url: moqURL, from: "0.2.0")

let package = Package(
    name: "smoke",
    platforms: [.macOS(.v12)],
    dependencies: [
        moqDependency,
    ],
    targets: [
        .executableTarget(
            name: "smoke",
            dependencies: [.product(name: "Moq", package: "moq-swift")],
            path: "Sources/smoke"
        ),
    ]
)
