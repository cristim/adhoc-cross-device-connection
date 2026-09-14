// main.swift — entry point for the macOS `ac-dc` helper.
//
// The macOS counterpart to the Linux `ac-dc` binary, implementing two
// subcommands:
//
//   send-key     Read the Continuity keys out of the keychain and push them to
//                a Linux box running `ac-dc receive-key`, over Bluetooth LE.
//                The keys stay in memory — nothing is written to disk.
//   export-keys  Write the same keys to keys.json (mode 0600) for the dual-boot
//                flow, where Linux reads them off a read-only APFS mount.

import Foundation

// Line-buffer stdout: the keychain query below blocks on a GUI prompt, and with
// the default block buffering (anything but a tty) the "what am I waiting for"
// messages sit in the buffer until exit, so a piped run looks like a silent hang.
setvbuf(stdout, nil, _IOLBF, 0)

let exe = (CommandLine.arguments.first as NSString?)?.lastPathComponent ?? "ac-dc"

// The FileVault-protected export dir, never a synced folder (see export-keys.sh).
let defaultKeysURL = FileManager.default.homeDirectoryForCurrentUser
    .appendingPathComponent("Library/Application Support/ac-dc/keys.json")

func usage() -> Never {
    print("""
    \(exe) — export this Mac's Continuity keys and send them to Linux over BLE.

    Usage:
      \(exe) send-key [--keys <path>] [--key-id <uuid>]
            Export the keys from the keychain and transfer them to a Linux host
            running `ac-dc receive-key`. The keys are held in memory only.
            --keys <path> sends an already-exported file instead (no keychain
            access, so no prompt at all).

      \(exe) export-keys [-o <path>] [--key-id <uuid>]
            Write the keys to a file (default:
            \(defaultKeysURL.path)) at mode 0600.

    --key-id fetches only that one key. Each key is a separate keychain query,
    so this means one access prompt instead of one per key.

    Compare the 6-digit security code shown here with the one on Linux before
    confirming the transfer there.
    """)
    exit(2)
}

/// Value of `flag` in `args`, or nil. Exits on a flag given without a value.
func option(_ flag: String, in args: [String]) -> String? {
    guard let i = args.firstIndex(of: flag) else { return nil }
    guard i + 1 < args.count else {
        print("\(exe): \(flag) needs a value")
        exit(2)
    }
    return args[i + 1]
}

func exportedKeys(id: String?) -> [ContinuityKey] {
    do {
        let keys = try KeychainExport.continuityKeys(id: id)
        for k in keys {
            let warn = KeychainExport.validKeyLengths.contains(k.key.count)
                ? "" : "  ⚠️ not a valid AES key size \(KeychainExport.validKeyLengths)"
            print("  \(k.id.isEmpty ? "(no identifier)" : k.id)  \(k.key.count) bytes\(warn)")
        }
        return keys
    } catch {
        print("Keychain export failed: \(error.localizedDescription)")
        exit(1)
    }
}

/// Keep Time Machine from copying the exported keys into a backup. Best effort:
/// an unmanaged failure here is not worth aborting an export over, but say so
/// rather than pretending the exclusion happened.
func excludeFromTimeMachine(_ dir: URL) {
    let p = Process()
    p.executableURL = URL(fileURLWithPath: "/usr/bin/tmutil")
    p.arguments = ["addexclusion", dir.path]
    p.standardOutput = FileHandle.nullDevice
    p.standardError = FileHandle.nullDevice
    do {
        try p.run()
        p.waitUntilExit()
        if p.terminationStatus != 0 {
            print("Note: could not exclude \(dir.path) from Time Machine backups.")
        }
    } catch {
        print("Note: could not run tmutil to exclude \(dir.path) from backups.")
    }
}

let args = CommandLine.arguments
guard args.count >= 2 else { usage() }

switch args[1] {
case "send-key":
    let payload: Data
    if let path = option("--keys", in: args) {
        let url = URL(fileURLWithPath: path)
        guard let data = try? Data(contentsOf: url), !data.isEmpty else {
            print("Could not read \(url.path).")
            exit(1)
        }
        print("Sending \(data.count) bytes from \(url.path).")
        payload = data
    } else {
        print("Exporting Continuity keys from the keychain…")
        let keys = exportedKeys(id: option("--key-id", in: args))
        do {
            payload = try KeychainExport.keysJSON(keys)
        } catch {
            print("Could not serialize keys: \(error.localizedDescription)")
            exit(1)
        }
        print("Exported \(keys.count) key(s), in memory only — nothing written to disk.")
    }

    print("Starting Bluetooth transfer.")
    Sender(payload: payload).run()

case "export-keys":
    let url = option("-o", in: args).map { URL(fileURLWithPath: $0) } ?? defaultKeysURL
    print("Exporting Continuity keys from the keychain…")
    let keys = exportedKeys(id: option("--key-id", in: args))

    do {
        let json = try KeychainExport.keysJSON(keys)
        let dir = url.deletingLastPathComponent()
        try FileManager.default.createDirectory(at: dir,
                                                withIntermediateDirectories: true,
                                                attributes: [.posixPermissions: 0o700])
        excludeFromTimeMachine(dir)
        // Create at 0600 in one step so the key bytes are never briefly
        // world-readable (an atomic write would rename over the mode).
        try? FileManager.default.removeItem(at: url)
        guard FileManager.default.createFile(atPath: url.path,
                                             contents: json,
                                             attributes: [.posixPermissions: 0o600]) else {
            print("Could not write \(url.path)")
            exit(1)
        }
    } catch {
        print("Could not write \(url.path): \(error.localizedDescription)")
        exit(1)
    }

    print("""

    Wrote \(keys.count) key(s) to \(url.path) (mode 0600).
    Arm the one-shot auto-wipe:  bash macos/export-keys.sh --arm-autowipe-only
    """)

default:
    usage()
}
