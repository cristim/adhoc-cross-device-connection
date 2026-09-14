// Tests for the keychain-item decoding and keys.json emission. The
// SecItemCopyMatching calls themselves need a real keychain and a user clicking
// an access prompt, so they are exercised by running `ac-dc export-keys`; what
// is covered here is everything that happens to an item once it comes back.

import XCTest
@testable import ac_dc

final class KeychainExportTests: XCTestCase {
    let aes128 = Data((0..<16).map { UInt8($0) })
    let aes256 = Data((0..<32).map { UInt8(0xB0 &+ $0) })

    private func plist(_ dict: [String: Any]) throws -> Data {
        try PropertyListSerialization.data(fromPropertyList: dict, format: .binary, options: 0)
    }

    func testParsesBinaryPlistItem() throws {
        let data = try plist(["keyData": aes256,
                              "keyIdentifier": "AAF1491D-846E-4193-981C-DC0CDA5CD194",
                              "lastUsedCounter": 42])
        let key = KeychainExport.parse(data: data, account: "ignored")
        XCTAssertEqual(key?.id, "AAF1491D-846E-4193-981C-DC0CDA5CD194")
        XCTAssertEqual(key?.key, aes256)
        XCTAssertEqual(key?.wrapped, false)
    }

    /// A wrapped key cannot decrypt adverts; it must be flagged so the caller
    /// drops it rather than shipping it to Linux as if it were usable.
    func testWrappedKeyIsFlagged() throws {
        let data = try plist(["keyData": aes256, "keyIdentifier": "X", "isWrappedKey": true])
        XCTAssertEqual(KeychainExport.parse(data: data, account: "")?.wrapped, true)
    }

    /// Real macOS 26.1 exports carry 32-byte AES-256 keys. The parser must hand
    /// them back whole: truncating to 16 would silently produce a wrong key.
    func testAES256KeyIsReturnedIntact() throws {
        let data = try plist(["keyData": aes256, "keyIdentifier": "X"])
        let key = KeychainExport.parse(data: data, account: "")
        XCTAssertEqual(key?.key.count, 32)
        XCTAssertEqual(key?.key, aes256)
    }

    /// When the plist has no keyIdentifier, the UUID comes from the account
    /// attribute, with the macOS prefix stripped.
    func testFallsBackToAccountAttributeForID() throws {
        let data = try plist(["keyData": aes128])
        let key = KeychainExport.parse(data: data,
                                       account: "handoff-decryption-key-AAF1491D-846E")
        XCTAssertEqual(key?.id, "AAF1491D-846E")
    }

    func testAccountWithoutKnownPrefixIsUsedVerbatim() {
        XCTAssertEqual(KeychainExport.fallbackID(account: "something-else"), "something-else")
    }

    /// Some macOS versions hand back the bare AES key rather than a plist.
    func testBareKeyBytesAreAccepted() {
        let key = KeychainExport.parse(data: aes128, account: "handoff-decryption-key-Z")
        XCTAssertEqual(key?.key, aes128)
        XCTAssertEqual(key?.id, "Z")
    }

    func testUnrecognisedItemIsRejected() {
        XCTAssertNil(KeychainExport.parse(data: Data("not a plist".utf8), account: "a"))
        XCTAssertNil(KeychainExport.parse(data: Data(), account: "a"))
    }

    func testHexAndBase64KeyDataAreDecoded() throws {
        let hexItem = try plist(["keyData": aes128.hexString, "keyIdentifier": "h"])
        XCTAssertEqual(KeychainExport.parse(data: hexItem, account: "")?.key, aes128)

        let prefixed = try plist(["keyData": "0x" + aes128.hexString, "keyIdentifier": "h"])
        XCTAssertEqual(KeychainExport.parse(data: prefixed, account: "")?.key, aes128)

        let b64 = try plist(["keyData": aes256.base64EncodedString(), "keyIdentifier": "b"])
        XCTAssertEqual(KeychainExport.parse(data: b64, account: "")?.key, aes256)
    }

    /// The emitted JSON must match what src/keystore.rs parses on Linux:
    /// { "keys": [ { "id": ..., "key": "<hex>" } ] }
    func testKeysJSONMatchesRustSchema() throws {
        let keys = [ContinuityKey(id: "A", key: aes256, wrapped: false),
                    ContinuityKey(id: "B", key: aes128, wrapped: false)]
        let json = try KeychainExport.keysJSON(keys)
        let obj = try JSONSerialization.jsonObject(with: json) as? [String: Any]
        let entries = try XCTUnwrap(obj?["keys"] as? [[String: String]])
        XCTAssertEqual(entries.count, 2)
        XCTAssertEqual(entries[0]["id"], "A")
        XCTAssertEqual(entries[0]["key"], aes256.hexString)
        XCTAssertEqual(entries[1]["key"], aes128.hexString)
        // Lowercase hex with no separators, and no stray key material fields.
        XCTAssertEqual(Set(entries[0].keys), ["id", "key"])
        XCTAssertEqual(entries[0]["key"], entries[0]["key"]?.lowercased())
        XCTAssertEqual(json.last, 0x0a, "file should end with a newline")
    }

    func testHexRoundTrip() {
        XCTAssertEqual(Data(hexString: aes256.hexString), aes256)
        XCTAssertEqual(aes128.hexString.count, 32)
        XCTAssertNil(Data(hexString: "abc"), "odd length must be rejected")
        XCTAssertNil(Data(hexString: "zz"), "non-hex must be rejected")
        XCTAssertNil(Data(hexString: ""), "empty must be rejected")
    }
}

extension KeychainExportTests {
    /// Regression: 32-byte AES-256 keys are the normal case on macOS 26.1 and
    /// must not be reported as unexpected. Apple's Platform Security guide
    /// specifies a 256-bit Handoff key.
    func testAES256IsAValidKeyLength() {
        XCTAssertTrue(KeychainExport.validKeyLengths.contains(32))
        XCTAssertTrue(KeychainExport.validKeyLengths.contains(16))
        XCTAssertFalse(KeychainExport.validKeyLengths.contains(20))
    }

    /// A bare 32-byte key with no plist wrapper must be accepted too.
    func testBareAES256KeyBytesAreAccepted() {
        XCTAssertEqual(KeychainExport.parse(data: aes256, account: "h")?.key, aes256)
    }
}
