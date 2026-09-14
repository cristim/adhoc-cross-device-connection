// main.swift — entry point for the macOS `ac-dc` helper.
//
// ⚠️ UNVALIDATED: not compiled/run from this repo (no Swift toolchain here).
// Build on macOS with `swift build -c release`; see README.md.
//
// This is the macOS counterpart to the Linux `ac-dc` binary, but it implements
// only the `send-key` subcommand: it reads the exported keys.json and pushes it
// to a Linux box running `ac-dc receive-key` over Bluetooth LE. Any other
// invocation prints usage.

import Foundation

func usage() -> Never {
    let exe = (CommandLine.arguments.first as NSString?)?.lastPathComponent ?? "ac-dc"
    print("""
    \(exe) — send exported Continuity keys to a Linux box over Bluetooth LE.

    Usage:
      \(exe) send-key       Transfer keys.json to a Linux host running
                            `ac-dc receive-key`. Reads:
                            ~/Library/Application Support/ac-dc/keys.json

    Compare the 6-digit security code shown here with the one on Linux before
    confirming the transfer there.
    """)
    exit(2)
}

let args = CommandLine.arguments
guard args.count >= 2, args[1] == "send-key" else {
    usage()
}

// keys.json lives inside the FileVault-protected Application Support dir (see
// macos/export-keys.sh), never in a synced folder.
let home = FileManager.default.homeDirectoryForCurrentUser
let keysURL = home
    .appendingPathComponent("Library/Application Support/ac-dc/keys.json")

guard let payload = try? Data(contentsOf: keysURL), !payload.isEmpty else {
    print("Could not read \(keysURL.path).")
    print("Export keys first with macos/export-keys.sh.")
    exit(1)
}

print("Loaded \(payload.count) bytes of keys.json; starting Bluetooth transfer.")
Sender(payload: payload).run()
