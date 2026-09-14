// Transfer.swift — transport-independent crypto/framing core of `ac-dc send-key`
// (the macOS SENDER). This mirrors the Rust reference in ../../../src/transfer.rs.
//
// ⚠️ UNVALIDATED: not compiled in this repo (no Swift toolchain on the Linux dev
// box). Build/run on macOS; fix whatever the compiler flags.
//
// ===========================================================================
// WIRE FORMAT  (mirrored verbatim from src/transfer.rs)
// ===========================================================================
//
// Roles: the RECEIVER (Linux, `receive-key`) is the GATT server; the SENDER
// (macOS, `send-key`) is the GATT central/client.
//
// All public keys are 32-byte X25519 public keys, little-endian as produced by
// `x25519_dalek::PublicKey::to_bytes()` (RFC 7748 u-coordinate encoding).
//
// Handshake (in order):
//   1. Receiver generates an ephemeral X25519 keypair and publishes its 32-byte
//      public key on the RECEIVER-PUBKEY characteristic (readable).
//   2. Sender generates an ephemeral X25519 keypair, READS the receiver pubkey,
//      then WRITES its own 32-byte public key to the SENDER-PUBKEY
//      characteristic.
//   3. Both sides compute ECDH: shared = X25519(own_secret, peer_public).
//   4. Both sides derive the session key:
//         session_key = HKDF-SHA512(
//             ikm  = shared,
//             salt = "ac-dc-key-transfer-v1",
//             info = "ac-dc session key" || receiver_pub || sender_pub)[..32]
//      Note the FIXED transcript order (receiver_pub then sender_pub),
//      independent of who is sending.
//   5. Both sides derive the SAS:
//         sas = ( u64_be( SHA-512(receiver_pub || sender_pub)[..8] ) )
//               mod 1_000_000, formatted as 6 zero-padded decimal digits.
//      Same fixed order. Shown on both screens; the human confirms they match.
//
// Payload:
//   6. Sender seals the keys.json bytes:
//         sealed = nonce(12) || ChaCha20-Poly1305_seal(session_key, nonce, plaintext, aad="")
//      nonce is 12 random bytes; the 16-byte Poly1305 tag is appended to the
//      ciphertext by the AEAD (RFC 8439). (This is exactly ChaChaPoly.SealedBox
//      .combined.)
//   7. Sender frames `sealed` for the GATT MTU. The framed byte stream is:
//         stream = u32_be(sealed.len()) || sealed
//      split into chunks of at most (max_frame - 2) bytes; each chunk is sent
//      as one GATT write / one frame:
//         frame = u16_be(chunk.len()) || chunk
//   8. Receiver reassembles the frames back into `sealed`, then (only after the
//      human confirms the SAS matched) opens it with the session key and writes
//      keys.json to disk at mode 0600.
// ===========================================================================

import Foundation
import CryptoKit

enum Transfer {
    static let hkdfSalt = Data("ac-dc-key-transfer-v1".utf8)
    static let hkdfInfoPrefix = Data("ac-dc session key".utf8)

    /// Derive the 32-byte session key from the ECDH shared secret, bound to the
    /// transcript (fixed order: receiver pubkey then sender pubkey).
    static func sessionKey(shared: SharedSecret,
                           receiverPub: Data,
                           senderPub: Data) -> SymmetricKey {
        var info = Data()
        info.append(hkdfInfoPrefix)
        info.append(receiverPub)
        info.append(senderPub)
        // HKDF over the raw X25519 output. CryptoKit's SharedSecret is the raw
        // 32-byte value; feed it as the IKM.
        let ikm = SymmetricKey(data: shared.withUnsafeBytes { Data($0) })
        return HKDF<SHA512>.deriveKey(inputKeyMaterial: ikm,
                                      salt: hkdfSalt,
                                      info: info,
                                      outputByteCount: 32)
    }

    /// Derive the 6-digit SAS from SHA-512(receiver_pub || sender_pub).
    static func sas(receiverPub: Data, senderPub: Data) -> String {
        var t = Data()
        t.append(receiverPub)
        t.append(senderPub)
        let digest = SHA512.hash(data: t)
        let first8 = Array(digest.prefix(8))
        var n: UInt64 = 0
        for b in first8 { n = (n << 8) | UInt64(b) } // big-endian
        return String(format: "%06u", n % 1_000_000)
    }

    /// Seal plaintext under `key`: returns nonce(12) || ciphertext+tag. This is
    /// exactly ChaChaPoly.SealedBox.combined.
    static func seal(key: SymmetricKey, plaintext: Data) throws -> Data {
        let box = try ChaChaPoly.seal(plaintext, using: key)
        return box.combined
    }

    /// Split a sealed payload into length-prefixed frames sized for the MTU.
    /// `maxFrame` includes the 2-byte length prefix.
    static func framePayload(sealed: Data, maxFrame: Int) -> [Data] {
        precondition(maxFrame > 2, "maxFrame must exceed the 2-byte length prefix")
        let chunkLen = maxFrame - 2

        // stream = u32_be(sealed.count) || sealed
        var stream = Data()
        let total = UInt32(sealed.count)
        stream.append(UInt8((total >> 24) & 0xff))
        stream.append(UInt8((total >> 16) & 0xff))
        stream.append(UInt8((total >> 8) & 0xff))
        stream.append(UInt8(total & 0xff))
        stream.append(sealed)

        var frames: [Data] = []
        var i = stream.startIndex
        while i < stream.endIndex {
            let end = stream.index(i, offsetBy: chunkLen, limitedBy: stream.endIndex) ?? stream.endIndex
            let chunk = stream[i..<end]
            var frame = Data()
            let n = UInt16(chunk.count)
            frame.append(UInt8((n >> 8) & 0xff))
            frame.append(UInt8(n & 0xff))
            frame.append(chunk)
            frames.append(frame)
            i = end
        }
        return frames
    }
}
