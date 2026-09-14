// KeychainExport.swift — read the Continuity BLE encryption keys straight out of
// the macOS keychain, so `ac-dc send-key` needs no prior export step.
//
// This is the Security.framework equivalent of macos/export-keys.sh, with two
// advantages over that script's Path A: `SecItemCopyMatching` with
// `kSecMatchLimitAll` returns EVERY matching item (the `security` CLI returns
// only the first), and `kSecAttrSynchronizableAny` covers the iCloud-synced
// copies. It needs neither SIP disabled nor Frida (export-keys.sh Path B).
//
// The item shape is documented in ../../../../src/keystore.rs: generic-password
// items with service "com.apple.continuity.encryption" whose data is a binary
// plist carrying `keyData`, `keyIdentifier` and `isWrappedKey`.

import Foundation

struct ContinuityKey {
    let id: String
    let key: Data
    let wrapped: Bool
}

enum KeychainExportError: LocalizedError {
    case noItems
    case denied(OSStatus)
    case queryFailed(OSStatus)
    case noUsableKeys(total: Int, wrapped: Int, unreadable: Int)

    var errorDescription: String? {
        switch self {
        case .noItems:
            return """
            No keychain items with service "com.apple.continuity.encryption".
            Is this Mac signed into the same Apple ID as your iPhone, with
            Handoff enabled (System Settings > General > AirDrop & Handoff)?
            """
        case .denied(let status):
            return """
            The keychain denied access (OSStatus \(status): \
            \(SecCopyErrorMessageString(status, nil) as String? ?? "unknown")).
            Answer "Always Allow" on the keychain prompt, or run this from a
            Terminal that has been granted access.
            """
        case .queryFailed(let status):
            return "SecItemCopyMatching failed (OSStatus \(status): " +
                "\(SecCopyErrorMessageString(status, nil) as String? ?? "unknown"))."
        case .noUsableKeys(let total, let wrapped, let unreadable):
            var why = "Found \(total) Continuity item(s) but no usable key.\n"
            if wrapped > 0 {
                why += """
                \(wrapped) were wrapped, which cannot decrypt BLE adverts. Toggle \
                Handoff off and on so rapportd re-derives them, then retry.
                """
            }
            if unreadable > 0 {
                why += """
                \(unreadable) item(s) carried no recognisable `keyData`. The item \
                layout may have changed in this macOS version — inspect it with \
                `security find-generic-password -s \(KeychainExport.service) -w` and compare \
                against macos/dump-to-keys.py.
                """
            }
            return why
        }
    }
}

enum KeychainExport {
    static let service = "com.apple.continuity.encryption"
    /// Account attribute prefix macOS uses, e.g.
    /// "handoff-decryption-key-AAF1491D-846E-4193-981C-DC0CDA5CD194".
    static let accountPrefix = "handoff-decryption-key-"
    /// Valid AES key sizes. Apple's Platform Security guide specifies a 256-bit
    /// key for Handoff and macOS 26.1 exports 32 bytes; 16 and 24 are accepted
    /// because src/gcm.rs selects the AES variant by key length, as the
    /// seemoo-lab reference does. Anything else means we parsed the wrong field.
    static let validKeyLengths = [16, 24, 32]

    /// Every usable (unwrapped) Continuity key in the keychain.
    ///
    /// Two passes on purpose: the legacy (file-based) keychain rejects
    /// `kSecMatchLimitAll` together with `kSecReturnData` (errSecParam -50), so
    /// we first enumerate the matching accounts, then fetch each item's data
    /// one at a time.
    /// Pass `id` (a key UUID, or the full account string) to fetch exactly that
    /// one key. That is a single keychain query and so a single access prompt,
    /// instead of one per key.
    static func continuityKeys(id: String? = nil) throws -> [ContinuityKey] {
        let accounts: [String]
        if let id {
            accounts = [id.hasPrefix(accountPrefix) ? id : accountPrefix + id]
        } else {
            accounts = try matchingAccounts()
        }

        // Deduplicate: the same key is often present once per sync'd device.
        var byID: [String: ContinuityKey] = [:]
        var wrappedCount = 0
        var unreadableCount = 0
        for account in accounts {
            guard let data = try itemData(account: account) else { continue }
            guard let parsed = parse(data: data, account: account) else {
                unreadableCount += 1
                continue
            }
            if parsed.wrapped {
                wrappedCount += 1
                continue
            }
            byID[parsed.id.isEmpty ? parsed.key.hexString : parsed.id] = parsed
        }

        let keys = Array(byID.values).sorted { $0.id < $1.id }
        guard !keys.isEmpty else {
            if id != nil { throw KeychainExportError.noItems }
            throw KeychainExportError.noUsableKeys(total: accounts.count,
                                                   wrapped: wrappedCount,
                                                   unreadable: unreadableCount)
        }
        return keys
    }

    /// Pass 1: the account attribute of every item for our service, in order and
    /// deduplicated.
    private static func matchingAccounts() throws -> [String] {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecMatchLimit as String: kSecMatchLimitAll,
            kSecReturnAttributes as String: true,
            // Continuity keys are kSecAttrSynchronizable; without this the query
            // silently skips the iCloud-synced copies.
            kSecAttrSynchronizable as String: kSecAttrSynchronizableAny,
        ]

        var result: CFTypeRef?
        try check(SecItemCopyMatching(query as CFDictionary, &result))

        guard let items = result as? [[String: Any]], !items.isEmpty else {
            throw KeychainExportError.noItems
        }

        var seen = Set<String>()
        var accounts: [String] = []
        for item in items {
            let account = item[kSecAttrAccount as String] as? String ?? ""
            if seen.insert(account).inserted { accounts.append(account) }
        }
        return accounts
    }

    /// Pass 2: the item data for one account. This is the call that triggers the
    /// keychain access prompt.
    private static func itemData(account: String) throws -> Data? {
        var query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecMatchLimit as String: kSecMatchLimitOne,
            kSecReturnData as String: true,
            kSecAttrSynchronizable as String: kSecAttrSynchronizableAny,
        ]
        if !account.isEmpty { query[kSecAttrAccount as String] = account }

        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        try check(status)
        return result as? Data
    }

    private static func check(_ status: OSStatus) throws {
        switch status {
        case errSecSuccess:
            return
        case errSecItemNotFound:
            throw KeychainExportError.noItems
        case errSecUserCanceled, errSecAuthFailed, errSecInteractionNotAllowed:
            throw KeychainExportError.denied(status)
        default:
            throw KeychainExportError.queryFailed(status)
        }
    }

    /// `{ "keys": [ { "id": ..., "key": "<hex>" } ] }` — the shape
    /// src/keystore.rs loads on the Linux side.
    static func keysJSON(_ keys: [ContinuityKey]) throws -> Data {
        let payload = ["keys": keys.map { ["id": $0.id, "key": $0.key.hexString] }]
        var json = try JSONSerialization.data(withJSONObject: payload,
                                              options: [.prettyPrinted, .sortedKeys])
        json.append(0x0a)
        return json
    }

    // MARK: - item parsing

    static func parse(data: Data, account: String) -> ContinuityKey? {
        guard !data.isEmpty else { return nil }

        // Normal case: the item data is a plist holding keyData + keyIdentifier.
        let plist = try? PropertyListSerialization.propertyList(from: data,
                                                               options: [],
                                                               format: nil)
        if let dict = plist as? [String: Any],
           let raw = dict["keyData"] ?? dict["v_Data"],
           let keyData = bytes(from: raw) {
            let id = dict["keyIdentifier"] as? String ?? fallbackID(account: account)
            let wrapped = dict["isWrappedKey"] as? Bool ?? false
            return ContinuityKey(id: id, key: keyData, wrapped: wrapped)
        }

        // Some macOS versions hand back the bare AES key instead of a plist.
        if validKeyLengths.contains(data.count) {
            return ContinuityKey(id: fallbackID(account: account), key: data, wrapped: false)
        }
        return nil
    }

    /// The account attribute embeds the key UUID; use it when the plist carries
    /// no `keyIdentifier`.
    static func fallbackID(account: String) -> String {
        account.hasPrefix(accountPrefix)
            ? String(account.dropFirst(accountPrefix.count))
            : account
    }

    /// `keyData` is normally plist <data>, but tooling on some versions stringifies
    /// it as hex or base64 (mirrors as_bytes() in macos/dump-to-keys.py).
    static func bytes(from value: Any) -> Data? {
        if let d = value as? Data { return d }
        guard let s = (value as? String)?.trimmingCharacters(in: .whitespacesAndNewlines) else {
            return nil
        }
        if let d = Data(hexString: s.hasPrefix("0x") ? String(s.dropFirst(2)) : s) { return d }
        return Data(base64Encoded: s)
    }
}

extension Data {
    var hexString: String { map { String(format: "%02x", $0) }.joined() }

    init?(hexString: String) {
        guard !hexString.isEmpty, hexString.count % 2 == 0 else { return nil }
        var out = Data(capacity: hexString.count / 2)
        var i = hexString.startIndex
        while i < hexString.endIndex {
            let j = hexString.index(i, offsetBy: 2)
            guard let b = UInt8(hexString[i..<j], radix: 16) else { return nil }
            out.append(b)
            i = j
        }
        self = out
    }
}
