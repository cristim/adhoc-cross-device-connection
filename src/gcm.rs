//! AES-GCM with the non-standard parameters Apple uses for Handoff / Universal
//! Clipboard BLE advertisements.
//!
//! Two things prevent us from using the `aes-gcm` crate directly:
//!   * the IV is only 2 bytes (the advertisement counter), not the usual 12, and
//!   * the authentication tag is truncated to a single byte.
//!
//! So we implement GCM from primitives per NIST SP 800-38D. The reference
//! behaviour we must match is CryptoSwift's `GCM(iv:authenticationTag:
//! additionalAuthenticatedData:mode:.detached)` as used by seemoo-lab's
//! handoff-ble-viewer (`BLEDecryptor.swift`).
//!
//! IMPORTANT: this path is derived from the spec for a sub-96-bit IV; it still
//! needs validation against a real captured packet from your own devices (see
//! README, "Validation"). The unit test below only checks internal consistency.

use aes::cipher::{BlockEncrypt, KeyInit};
use aes::Aes128;
use ghash::universal_hash::UniversalHash;
use ghash::GHash;

const BLOCK: usize = 16;

/// Encrypt one AES-128 block (ECB core), used for GHASH key H and CTR blocks.
fn aes_block(cipher: &Aes128, input: &[u8; BLOCK]) -> [u8; BLOCK] {
    let mut b = aes::cipher::generic_array::GenericArray::clone_from_slice(input);
    cipher.encrypt_block(&mut b);
    let mut out = [0u8; BLOCK];
    out.copy_from_slice(&b);
    out
}

fn xor_into(dst: &mut [u8], src: &[u8]) {
    for (d, s) in dst.iter_mut().zip(src) {
        *d ^= *s;
    }
}

/// GHASH over a sequence of already-zero-padded 16-byte blocks.
fn ghash(h: &[u8; BLOCK], blocks: &[[u8; BLOCK]]) -> [u8; BLOCK] {
    let mut mac = GHash::new(h.into());
    for blk in blocks {
        mac.update(&[(*blk).into()]);
    }
    mac.finalize().into()
}

/// Right-pad `data` with zeros to a whole number of 16-byte blocks.
fn pad_blocks(data: &[u8], out: &mut Vec<[u8; BLOCK]>) {
    for chunk in data.chunks(BLOCK) {
        let mut b = [0u8; BLOCK];
        b[..chunk.len()].copy_from_slice(chunk);
        out.push(b);
    }
}

/// Build J0 for an IV whose length is not 96 bits (SP 800-38D §7.1, step 2b):
///   J0 = GHASH_H(IV || 0^(s+64) || [len(IV)]_64)
fn j0_short_iv(h: &[u8; BLOCK], iv: &[u8]) -> [u8; BLOCK] {
    let mut blocks: Vec<[u8; BLOCK]> = Vec::new();
    pad_blocks(iv, &mut blocks);
    let mut len_block = [0u8; BLOCK];
    let bit_len = (iv.len() as u64) * 8;
    len_block[8..].copy_from_slice(&bit_len.to_be_bytes());
    blocks.push(len_block);
    ghash(h, &blocks)
}

fn inc32(mut j: [u8; BLOCK]) -> [u8; BLOCK] {
    let mut ctr = u32::from_be_bytes([j[12], j[13], j[14], j[15]]);
    ctr = ctr.wrapping_add(1);
    j[12..].copy_from_slice(&ctr.to_be_bytes());
    j
}

/// Decrypt-and-verify with a truncated tag.
///
/// * `key` – 16-byte AES-128 key (`keyData` from the keychain item).
/// * `iv` – advertisement counter bytes (little-endian on the wire; we feed
///   them here exactly as they appear in the packet).
/// * `aad` – the plaintext status byte.
/// * `ciphertext` – the encrypted payload (10 bytes for a Handoff advert).
/// * `tag` – the truncated authentication tag from the packet (1 byte).
///
/// Returns the plaintext if the truncated tag matches, else `None`.
pub fn open_truncated(
    key: &[u8],
    iv: &[u8],
    aad: &[u8],
    ciphertext: &[u8],
    tag: &[u8],
) -> Option<Vec<u8>> {
    if key.len() != 16 {
        return None;
    }
    let cipher = Aes128::new(aes::cipher::generic_array::GenericArray::from_slice(key));

    // H = E_K(0^128)
    let h = aes_block(&cipher, &[0u8; BLOCK]);

    // J0 from the short IV.
    let j0 = j0_short_iv(&h, iv);

    // S = GHASH_H(A_padded || C_padded || [len(A)]_64 || [len(C)]_64)
    let mut blocks: Vec<[u8; BLOCK]> = Vec::new();
    pad_blocks(aad, &mut blocks);
    pad_blocks(ciphertext, &mut blocks);
    let mut len_block = [0u8; BLOCK];
    len_block[..8].copy_from_slice(&((aad.len() as u64) * 8).to_be_bytes());
    len_block[8..].copy_from_slice(&((ciphertext.len() as u64) * 8).to_be_bytes());
    blocks.push(len_block);
    let s = ghash(&h, &blocks);

    // Full tag = E_K(J0) XOR S, then truncate to the length we were given.
    let mut full_tag = aes_block(&cipher, &j0);
    xor_into(&mut full_tag, &s);

    if tag.is_empty() || tag.len() > BLOCK {
        return None;
    }
    if full_tag[..tag.len()] != *tag {
        return None;
    }

    // CTR decrypt from inc32(J0).
    let mut plaintext = Vec::with_capacity(ciphertext.len());
    let mut counter = inc32(j0);
    for chunk in ciphertext.chunks(BLOCK) {
        let ks = aes_block(&cipher, &counter);
        let mut block = chunk.to_vec();
        xor_into(&mut block, &ks[..chunk.len()]);
        plaintext.extend_from_slice(&block);
        counter = inc32(counter);
    }
    Some(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip: encrypt with the same primitive path, confirm the truncated
    /// tag verifies and the plaintext comes back. This proves the GHASH/CTR/J0
    /// machinery is internally consistent; it does NOT prove we match Apple's
    /// exact framing (that requires a captured packet).
    #[test]
    fn truncated_roundtrip() {
        let key = [0x11u8; 16];
        let iv = [0x00u8, 0x2a];
        let aad = [0x08u8];
        let plaintext = b"0123456789"; // 10 bytes, like a Handoff payload

        // Encrypt by hand using the same helpers.
        let cipher = Aes128::new(aes::cipher::generic_array::GenericArray::from_slice(&key));
        let h = aes_block(&cipher, &[0u8; BLOCK]);
        let j0 = j0_short_iv(&h, &iv);
        let mut ct = Vec::new();
        let mut counter = inc32(j0);
        for chunk in plaintext.chunks(BLOCK) {
            let ks = aes_block(&cipher, &counter);
            let mut b = chunk.to_vec();
            xor_into(&mut b, &ks[..chunk.len()]);
            ct.extend_from_slice(&b);
            counter = inc32(counter);
        }
        let mut blocks: Vec<[u8; BLOCK]> = Vec::new();
        pad_blocks(&aad, &mut blocks);
        pad_blocks(&ct, &mut blocks);
        let mut len_block = [0u8; BLOCK];
        len_block[..8].copy_from_slice(&((aad.len() as u64) * 8).to_be_bytes());
        len_block[8..].copy_from_slice(&((ct.len() as u64) * 8).to_be_bytes());
        blocks.push(len_block);
        let s = ghash(&h, &blocks);
        let mut full_tag = aes_block(&cipher, &j0);
        xor_into(&mut full_tag, &s);
        let tag = [full_tag[0]]; // 1-byte truncated tag

        let out = open_truncated(&key, &iv, &aad, &ct, &tag).expect("tag should verify");
        assert_eq!(out, plaintext);

        // A wrong tag must be rejected.
        assert!(open_truncated(&key, &iv, &aad, &ct, &[tag[0] ^ 0xff]).is_none());
    }
}
