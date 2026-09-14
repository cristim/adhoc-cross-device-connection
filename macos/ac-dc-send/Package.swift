// swift-tools-version:5.7
// SwiftPM manifest for the macOS `ac-dc send-key` helper.
//
// ⚠️ UNVALIDATED: this package has NOT been compiled or run on a Mac from this
// repo checkout (there is no Swift toolchain in the Linux dev environment where
// it was written). Build it on macOS with `swift build -c release`; see
// README.md. Fix anything the compiler flags — treat it as a careful draft.

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
        )
    ]
)
