// Cross-implementation vectors: the Swift sender and the Rust receiver
// (src/transfer.rs) must agree byte for byte, or the transfer fails or the two
// screens show different security codes.
//
// The expected values below were produced by the Rust implementation. Regenerate
// them from src/transfer.rs if the wire format ever changes deliberately; do not
// "fix" a failure here by editing the expectation, because a mismatch means the
// two ends have actually diverged.

import XCTest
import CryptoKit
@testable import ac_dc

final class TransferInteropTests: XCTestCase {
    // Fixed synthetic inputs, matching the Rust generator.
    let shared = Data((0..<32).map { UInt8($0) })
    let receiverPub = Data((0..<32).map { UInt8(0x40 + $0) })
    let senderPub = Data((0..<32).map { UInt8(0x80 + $0) })

    func testSessionKeyMatchesRustVector() {
        let key = Transfer.sessionKey(sharedBytes: shared,
                                      receiverPub: receiverPub,
                                      senderPub: senderPub)
        let hex = key.withUnsafeBytes { Data($0) }.hexString
        XCTAssertEqual(hex, "9ef76a0dfa509e92f8a739872a202cb305609d047717a99fb063fc98f4e51325")
    }

    func testSASMatchesRustVector() {
        XCTAssertEqual(Transfer.sas(receiverPub: receiverPub, senderPub: senderPub), "146572")
    }

    /// The SAS is what defends the untrusted BLE link, so a swapped transcript
    /// order must produce a different code.
    func testSASIsOrderDependent() {
        XCTAssertNotEqual(Transfer.sas(receiverPub: receiverPub, senderPub: senderPub),
                          Transfer.sas(receiverPub: senderPub, senderPub: receiverPub))
    }

    func testSASIsAlwaysSixDigits() {
        for i in 0..<200 {
            let a = Data((0..<32).map { _ in UInt8(i & 0xff) })
            let b = Data((0..<32).map { UInt8(($0 &+ i) & 0xff) })
            let sas = Transfer.sas(receiverPub: a, senderPub: b)
            XCTAssertEqual(sas.count, 6, "SAS \(sas) is not 6 characters")
            XCTAssertTrue(sas.allSatisfy(\.isNumber), "SAS \(sas) is not all digits")
        }
    }

    func testFramingMatchesRustVectors() {
        // 70-byte payload at maxFrame 20: several full chunks, a short final
        // chunk, and the u32 length prefix inside the first chunk.
        let sealed = Data((0..<70).map { UInt8($0) })
        let frames = Transfer.framePayload(sealed: sealed, maxFrame: 20)
        XCTAssertEqual(frames.map(\.hexString), [
            "001200000046000102030405060708090a0b0c0d",
            "00120e0f101112131415161718191a1b1c1d1e1f",
            "0012202122232425262728292a2b2c2d2e2f3031",
            "001232333435363738393a3b3c3d3e3f40414243",
            "00024445",
        ])
    }

    /// Every frame must fit the MTU, and the frames must reassemble to
    /// u32_be(len) || sealed for a range of payload sizes and MTUs.
    func testFramingRespectsMTUAndReassembles() {
        for size in [0, 1, 17, 18, 19, 255, 256, 1024] {
            for maxFrame in [3, 4, 20, 185, 512] {
                let sealed = Data((0..<size).map { UInt8($0 & 0xff) })
                let frames = Transfer.framePayload(sealed: sealed, maxFrame: maxFrame)
                var stream = Data()
                for f in frames {
                    XCTAssertLessThanOrEqual(f.count, maxFrame, "frame exceeds MTU \(maxFrame)")
                    let declared = Int(f[f.startIndex]) << 8 | Int(f[f.startIndex + 1])
                    XCTAssertEqual(declared, f.count - 2, "length prefix disagrees with frame")
                    stream.append(f.dropFirst(2))
                }
                var expected = Data([UInt8(size >> 24 & 0xff), UInt8(size >> 16 & 0xff),
                                     UInt8(size >> 8 & 0xff), UInt8(size & 0xff)])
                expected.append(sealed)
                XCTAssertEqual(stream, expected, "size=\(size) maxFrame=\(maxFrame)")
            }
        }
    }

    /// ChaChaPoly.combined must be nonce(12) || ciphertext || tag(16), which is
    /// what the Rust `open` expects.
    func testSealedLayoutMatchesRustExpectation() throws {
        let key = SymmetricKey(data: Data((0..<32).map { UInt8(0xA0 &+ $0) }))
        let plaintext = Data("{\"keys\":[]}\n".utf8)
        let sealed = try Transfer.seal(key: key, plaintext: plaintext)
        XCTAssertEqual(sealed.count, 12 + plaintext.count + 16)
        // Reopening with the same key must recover the plaintext.
        let box = try ChaChaPoly.SealedBox(combined: sealed)
        XCTAssertEqual(try ChaChaPoly.open(box, using: key), plaintext)
    }

    /// A fresh nonce per call: reusing one under the same key would be fatal for
    /// ChaCha20-Poly1305.
    func testSealUsesAFreshNoncePerCall() throws {
        let key = SymmetricKey(data: Data(repeating: 7, count: 32))
        let plaintext = Data("same plaintext".utf8)
        var nonces = Set<Data>()
        for _ in 0..<50 {
            let sealed = try Transfer.seal(key: key, plaintext: plaintext)
            nonces.insert(sealed.prefix(12))
        }
        XCTAssertEqual(nonces.count, 50, "nonce repeated across seals")
    }
}
