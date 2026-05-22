// swift-tools-version:5.9
//
// Swift Package Manager manifest for Moq.
//
// The MoqFFI binary target points at an XCFramework attached to the
// matching moq-ffi-v* GitHub Release. swift/scripts/package.sh rewrites
// the URL and checksum below as part of the release pipeline before this
// file is pushed to the moq-dev/moq-swift mirror repo, which is what SPM
// consumers actually resolve.
//
// For local development pre-release, swap MoqFFI's `.binaryTarget` for
// `.binaryTarget(name: "MoqFFI", path: "MoqFFI.xcframework")`.

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
        .binaryTarget(
            name: "MoqFFI",
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
