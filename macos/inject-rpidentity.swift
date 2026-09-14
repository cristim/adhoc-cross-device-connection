// inject-rpidentity.swift — approach (b): mint a fresh long-term Ed25519 identity
// for ac-dc and INJECT its PUBLIC half into the iCloud keychain as a new
// `RPIdentity-SameAccountDevice` item, so that every other same-Apple-ID device
// accepts us as a trusted device during companion-link Pair-Verify.
//
// This is the faithful modern (CryptoKit) port of seemoo-lab
// handoff-authentication-swift MacKeychainController.createNewRPIdentityItem:
// it SecItemAdds a synchronizable generic-password whose value is an OPACK blob
// { edPK: <32-byte pubkey>, dIRK: <16-byte device IRK> }. We keep the PRIVATE
// seed locally (it never enters the keychain) and hand it to ac-dc via
// rpidentity.json.
//
// The plain `security` CLI cannot create a *synchronizable* item with *binary*
// value data and the extra protection attributes (`pdmn`, `tomb`, syncViewHint),
// which is why this small Security.framework helper exists. Reading peer keys
// still uses the security-CLI / Frida paths in export-rpidentity.sh.
//
// Run:   swift inject-rpidentity.swift --label "ac-dc Linux" --out identity-self.json
//        swift inject-rpidentity.swift --seed <64hex> --dirk <32hex> ...   (re-inject a known key)
//        swift inject-rpidentity.swift --dry-run ...                        (mint + print, DO NOT touch keychain)
//
// ⚠️ UNVERIFIED against a real iCloud keychain. SecItemAdd of a synchronizable
// item in the `com.apple.rapport` access group may require that entitlement and
// be refused with errSecMissingEntitlement (-34018); the fallback below adds the
// item with no access group (seemoo's "Eve's little brother" case). Whether a
// no-access-group RPIdentity item is honoured by peers is the open trust
// question — see macos/RPIDENTITY.md.

import Foundation
import Security
import CryptoKit

// ---- tiny arg parsing ----
func argValue(_ name: String) -> String? {
    let a = CommandLine.arguments
    if let i = a.firstIndex(of: name), i + 1 < a.count { return a[i + 1] }
    return nil
}
func hasFlag(_ name: String) -> Bool { CommandLine.arguments.contains(name) }

let label = argValue("--label") ?? "ac-dc"
let outPath = argValue("--out") ?? "identity-self.json"
let dryRun = hasFlag("--dry-run")
let noAccessGroup = hasFlag("--no-access-group")

func hexToData(_ s: String) -> Data? {
    var s = s.trimmingCharacters(in: .whitespacesAndNewlines)
    s = s.replacingOccurrences(of: " ", with: "")
    if s.hasPrefix("<") && s.hasSuffix(">") { s = String(s.dropFirst().dropLast()) }
    guard s.count % 2 == 0 else { return nil }
    var d = Data(); var idx = s.startIndex
    while idx < s.endIndex {
        let next = s.index(idx, offsetBy: 2)
        guard let b = UInt8(s[idx..<next], radix: 16) else { return nil }
        d.append(b); idx = next
    }
    return d
}

// ---- key material ----
let seedData: Data
if let seedHex = argValue("--seed") {
    // A libsodium edSK is 64 bytes (seed32||pub32); we accept either and keep 32.
    guard let d = hexToData(seedHex), d.count == 32 || d.count == 64 else {
        FileHandle.standardError.write("error: --seed must be 32 or 64 hex bytes\n".data(using: .utf8)!)
        exit(2)
    }
    seedData = d.prefix(32)
} else {
    seedData = Curve25519.Signing.PrivateKey().rawRepresentation // 32-byte seed
}
let signingKey = try! Curve25519.Signing.PrivateKey(rawRepresentation: seedData)
let edPK = signingKey.publicKey.rawRepresentation // 32 bytes
let edSKseed = signingKey.rawRepresentation        // 32 bytes (what ed25519-dalek wants)

let dirkData: Data
if let dirkHex = argValue("--dirk") {
    guard let d = hexToData(dirkHex), d.count == 16 else {
        FileHandle.standardError.write("error: --dirk must be 16 hex bytes\n".data(using: .utf8)!)
        exit(2)
    }
    dirkData = d
} else {
    var b = [UInt8](repeating: 0, count: 16)
    _ = SecRandomCopyBytes(kSecRandomDefault, 16, &b)
    dirkData = Data(b)
}

// ---- OPACK encode { "edPK": <bytes>, "dIRK": <bytes> } ----
// Subset of Apple's OPACK, matching ../src/opack.rs and seemoo OPACKCoding:
//   dict(n<15) = 0xE0+n ; str(len<=0x20) = 0x40+len + utf8 ; bytes(len<=0x20) = 0x70+len + data
func opackStr(_ s: String) -> Data {
    let u = Array(s.utf8)
    precondition(u.count <= 0x20, "key too long for short OPACK string")
    return Data([0x40 + UInt8(u.count)] + u)
}
func opackBytes(_ b: Data) -> Data {
    precondition(b.count <= 0x20, "value too long for short OPACK bytes")
    return Data([0x70 + UInt8(b.count)]) + b
}
var opack = Data([0xE0 + 2]) // dict, 2 pairs
opack += opackStr("edPK"); opack += opackBytes(edPK)
opack += opackStr("dIRK"); opack += opackBytes(dirkData)

let account = UUID().uuidString

// ---- write our identity to disk (private seed stays local, NOT in keychain) ----
func writeSelf() {
    let obj: [String: Any] = [
        "ed_sk": edSKseed.map { String(format: "%02x", $0) }.joined(),
        "edpk": edPK.map { String(format: "%02x", $0) }.joined(),
        "dirk": dirkData.map { String(format: "%02x", $0) }.joined(),
        "account": account,
        "label": label,
    ]
    let data = try! JSONSerialization.data(withJSONObject: obj, options: [.prettyPrinted, .sortedKeys])
    try! data.write(to: URL(fileURLWithPath: outPath))
    // owner-only
    try? FileManager.default.setAttributes([.posixPermissions: 0o600],
                                           ofItemAtPath: outPath)
    FileHandle.standardError.write("Wrote \(outPath) (ed_sk=OUR 32-byte seed, edpk, dirk, mode 600)\n".data(using: .utf8)!)
}

if dryRun {
    FileHandle.standardError.write("--dry-run: minted identity, NOT touching the keychain.\n".data(using: .utf8)!)
    writeSelf()
    exit(0)
}

// ---- SecItemAdd (synchronizable, iCloud-synced) ----
func attributes(withAccessGroup group: String?) -> [String: Any] {
    var a: [String: Any] = [
        kSecAttrService as String: "RPIdentity-SameAccountDevice",
        kSecAttrAccount as String: account,
        kSecClass as String: kSecClassGenericPassword,
        kSecAttrSynchronizable as String: true,
        kSecAttrLabel as String: label,
        kSecAttrSyncViewHint as String: "Home",
        "pdmn": "cku",   // kSecAttrAccessible == AfterFirstUnlock (protection domain)
        "tomb": 0,
        kSecValueData as String: opack,
    ]
    if let g = group { a[kSecAttrAccessGroup as String] = g }
    return a
}

func add(_ attrs: [String: Any]) -> OSStatus { SecItemAdd(attrs as CFDictionary, nil) }

var status = add(attributes(withAccessGroup: noAccessGroup ? nil : "com.apple.rapport"))
if status == errSecMissingEntitlement || status == errSecParam {
    FileHandle.standardError.write(
        "SecItemAdd with access group com.apple.rapport failed (\(status)); retrying with no access group.\n"
        .data(using: .utf8)!)
    status = add(attributes(withAccessGroup: nil))
}

guard status == errSecSuccess else {
    let msg = SecCopyErrorMessageString(status, nil) as String? ?? "unknown"
    FileHandle.standardError.write("SecItemAdd FAILED: OSStatus \(status) (\(msg))\n".data(using: .utf8)!)
    FileHandle.standardError.write(
        "Common causes: -34018 missing entitlement (try --no-access-group), or the\n"
        + "item already exists. Nothing was written to \(outPath).\n"
        .data(using: .utf8)!)
    exit(1)
}

FileHandle.standardError.write("Injected RPIdentity-SameAccountDevice item (account \(account), label \"\(label)\").\n".data(using: .utf8)!)
writeSelf()
print(account)
