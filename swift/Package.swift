// swift-tools-version:5.9
//
// Swift Package Manager manifest for Moq.
//
// Three-target layout:
//   - MoqFFIBinary: the XCFramework attached to the matching
//     moq-ffi-v* GitHub Release. Provides the C `moqFFI` clang module
//     (low-level uniffi scaffolding).
//   - MoqFFI: Swift wrapper that compiles the uniffi-generated
//     `Sources/MoqFFI/Generated.swift`, which `import moqFFI` and
//     exposes Swift-native types (MoqClient, MoqSession, etc.).
//   - Moq: hand-written ergonomic API on top of MoqFFI.
//
// swift/scripts/package.sh rewrites the URL and checksum below before
// this file is pushed to the moq-dev/moq-swift mirror repo, which is
// what SPM consumers actually resolve. For local pre-release dev,
// swift/scripts/check.sh swaps the URL-based binaryTarget for a path-
// based one pointing at MoqFFI.xcframework next to this manifest.

import PackageDescription

let package = Package(
    name: "Moq",
    platforms: [
        .iOS(.v15),
        .macOS(.v12),
    ],
    products: [
        .library(name: "Moq", targets: ["Moq"]),
    ],
    targets: [
        .target(
            name: "Moq",
            dependencies: ["MoqFFI"],
            path: "Sources/Moq"
        ),
        .target(
            name: "MoqFFI",
            dependencies: ["MoqFFIBinary"],
            path: "Sources/MoqFFI"
        ),
        .binaryTarget(
            name: "MoqFFIBinary",
            url: "https://github.com/moq-dev/moq/releases/download/moq-ffi-vREPLACE_VERSION/MoqFFI.xcframework.zip",
            checksum: "REPLACE_CHECKSUM"
        ),
        .testTarget(
            name: "MoqTests",
            dependencies: ["Moq"],
            path: "Tests/MoqTests"
        ),
    ]
)
