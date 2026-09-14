// swift-tools-version:5.7
// SwiftPM manifest for the macOS `ac-dc send-key` helper.
//
// Build on macOS with `swift build -c release`, then `swift test`. The package
// has been built and run on macOS 26.1, including a real BLE transfer to a Linux
// receiver; see README.md.

import PackageDescription

let package = Package(
    name: "ac-dc-send",
    platforms: [
        // CoreBluetooth central + CryptoKit (X25519, HKDF-SHA512, ChaChaPoly).
        .macOS(.v11)
    ],
    targets: [
        .executableTarget(
            name: "ac-dc",
            path: "Sources/ac-dc"
        ),
        .testTarget(
            name: "ac-dcTests",
            dependencies: ["ac-dc"],
            path: "Tests/ac-dcTests"
        )
    ]
)
