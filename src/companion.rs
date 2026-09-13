//! Milestone 2: the companion-link service that carries the actual clipboard
//! content, after the BLE advert tells us a copy is available.
//!
//! Flow (from Stute et al. 2021 and seemoo-lab handoff-authentication-swift):
//!   1. discover `_companion-link._tcp` over mDNS (see `discover.rs`)
//!   2. TCP connect, run Pair-Verify (Curve25519 ECDH + Ed25519 signatures over
//!      the long-term `RPIdentity-SameAccountDevice` keys)
//!   3. derive per-direction keys and run a ChaCha20-Poly1305 channel
//!   4. exchange OPACK payloads; the clipboard content comes back in one.
//!
//! ⚠️ NONE of this is validated against a real device. Two things in particular
//! are unverified and marked TODO below:
//!   * the exact ChaCha20-Poly1305 nonce construction for Pair-Verify (the
//!     reference used a CoreCrypto "64x64" call; we implement the RFC 8439 /
//!     HAP-style `0x00000000 || label` nonce, which is the most likely match);
//!   * the real key material, which only arrives once `RPIdentity` keys are
//!     exported from macOS (see `PairingIdentity` / `load_identity`).

#![allow(dead_code)] // M2/M3 scaffolding: exercised by unit tests; wired into the runtime once macOS keys exist.

use anyhow::{bail, Context, Result};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use serde::Deserialize;
use sha2::Sha512;
use std::path::Path;
use x25519_dalek::{EphemeralSecret, PublicKey};

use crate::opack::{self, Value};
use crate::tlv8::Tlv8;

// ---------------------------------------------------------------------------
// Key derivation
// ---------------------------------------------------------------------------

/// HKDF-SHA512, mirroring Apple's CoreUtils `CryptoHKDF` (extract + expand).
/// An empty salt is treated as all-zeros per RFC 5869.
pub fn hkdf_sha512(secret: &[u8], salt: &[u8], info: &[u8], out_len: usize) -> Vec<u8> {
    let hk = Hkdf::<Sha512>::new(Some(salt), secret);
    let mut okm = vec![0u8; out_len];
    hk.expand(info, &mut okm).expect("HKDF output length within SHA-512 limit");
    okm
}

const KEY_LEN: usize = 32;

// ---------------------------------------------------------------------------
// ChaCha20-Poly1305 content channel
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Client,
    Server,
}

/// The post-Pair-Verify encrypted channel. Keys are derived from the ECDH
/// shared secret with the info strings from `HandoffCryptor.swift`; each
/// direction has its own key and a 64-bit little-endian counter nonce
/// zero-padded to the 96-bit RFC 8439 nonce.
pub struct ContentChannel {
    enc_key: [u8; KEY_LEN],
    dec_key: [u8; KEY_LEN],
    enc_nonce: u64,
    dec_nonce: u64,
}

impl ContentChannel {
    pub fn from_shared_secret(secret: &[u8], role: Role) -> Self {
        let server = hkdf_sha512(secret, b"", b"ServerEncrypt-main", KEY_LEN);
        let client = hkdf_sha512(secret, b"", b"ClientEncrypt-main", KEY_LEN);
        let (enc, dec) = match role {
            Role::Client => (client, server),
            Role::Server => (server, client),
        };
        let mut enc_key = [0u8; KEY_LEN];
        let mut dec_key = [0u8; KEY_LEN];
        enc_key.copy_from_slice(&enc);
        dec_key.copy_from_slice(&dec);
        ContentChannel { enc_key, dec_key, enc_nonce: 0, dec_nonce: 0 }
    }

    fn nonce(counter: u64) -> Nonce {
        // 8-byte LE counter, then 4 zero bytes => 12-byte RFC 8439 nonce.
        let mut n = [0u8; 12];
        n[..8].copy_from_slice(&counter.to_le_bytes());
        *Nonce::from_slice(&n)
    }

    pub fn encrypt(&mut self, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.enc_key));
        let out = cipher
            .encrypt(&Self::nonce(self.enc_nonce), Payload { msg: plaintext, aad })
            .map_err(|_| anyhow::anyhow!("chacha encrypt failed"))?;
        self.enc_nonce += 1;
        Ok(out)
    }

    pub fn decrypt(&mut self, ciphertext: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.dec_key));
        let out = cipher
            .decrypt(&Self::nonce(self.dec_nonce), Payload { msg: ciphertext, aad })
            .map_err(|_| anyhow::anyhow!("chacha decrypt failed / bad tag"))?;
        self.dec_nonce += 1;
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// ContinuityPacket framing (4-byte header + body)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PacketType {
    PairVerifyPublicKey = 0x05,
    PairVerifyContinue = 0x06,
    EncryptedData = 0x08,
    Finished = 0x01,
}

impl PacketType {
    fn from_u8(b: u8) -> Option<Self> {
        Some(match b {
            0x05 => PacketType::PairVerifyPublicKey,
            0x06 => PacketType::PairVerifyContinue,
            0x08 => PacketType::EncryptedData,
            0x01 => PacketType::Finished,
            _ => return None,
        })
    }
}

/// `type(1) | 0x00 | bodylen_be(2) | body`. For EncryptedData the reference
/// adds 16 to the advertised length to account for the Poly1305 tag.
pub struct ContinuityPacket {
    pub ptype: PacketType,
    pub body: Vec<u8>,
}

impl ContinuityPacket {
    pub fn new(ptype: PacketType, body: Vec<u8>) -> Self {
        ContinuityPacket { ptype, body }
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut adv_len = self.body.len();
        if self.ptype == PacketType::EncryptedData {
            adv_len += 16;
        }
        let mut out = Vec::with_capacity(4 + self.body.len());
        out.push(self.ptype as u8);
        out.push(0x00);
        out.extend_from_slice(&(adv_len as u16).to_be_bytes());
        out.extend_from_slice(&self.body);
        out
    }

    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 4 {
            bail!("continuity packet shorter than header");
        }
        let ptype = PacketType::from_u8(data[0])
            .with_context(|| format!("unknown packet type {:#04x}", data[0]))?;
        Ok(ContinuityPacket { ptype, body: data[4..].to_vec() })
    }

    /// Body is `OPACK({ "_pd": <TLV8 bytes> })`. Extract and decode the TLV.
    pub fn pairing_tlv(&self) -> Result<Tlv8> {
        let dict = opack::decode(&self.body)?;
        let pd = dict.get("_pd").and_then(Value::as_bytes).context("no _pd in packet")?;
        Ok(Tlv8::decode(pd))
    }
}

fn wrap_pairing_data(tlv: &Tlv8) -> Vec<u8> {
    opack::encode(&Value::dict([("_pd", Value::Bytes(tlv.encode()))]))
}

// Pair-Verify TLV types (from PairingSession.swift PairingTLV).
mod tlv_type {
    pub const IDENTITY_ID: u8 = 0x01;
    pub const PUBLIC_KEY: u8 = 0x03;
    pub const ENCRYPTED_DATA: u8 = 0x05;
    pub const STATE: u8 = 0x06;
    pub const SIGNATURE: u8 = 0x0A;
    pub const APP_FLAGS: u8 = 0x19;
}

// ---------------------------------------------------------------------------
// Long-term pairing identity (from RPIdentity-SameAccountDevice)
// ---------------------------------------------------------------------------

/// Our own device identity plus, for peer verification, the peers' Ed25519
/// public keys. The Ed25519 secret and the peer public keys are iCloud-synced
/// keychain material; they arrive from an export like keys.json.
///
/// TODO(keys): wire this to a real exporter (`macos/export-keys.sh` currently
/// only pulls the BLE key; RPIdentity export is a follow-up). The JSON schema
/// here is provisional.
pub struct PairingIdentity {
    pub signing: SigningKey,
    pub device_irk: [u8; 16],
    /// Known peer devices' Ed25519 verifying keys (edPK), by label.
    pub peers: Vec<(String, VerifyingKey)>,
}

#[derive(Deserialize)]
struct IdentityFile {
    /// 32-byte Ed25519 seed (hex).
    ed_sk: String,
    /// 16-byte device IRK (hex).
    #[serde(default)]
    dirk: String,
    #[serde(default)]
    peers: Vec<PeerFile>,
}

#[derive(Deserialize)]
struct PeerFile {
    #[serde(default)]
    label: String,
    /// 32-byte Ed25519 public key (hex).
    edpk: String,
}

impl PairingIdentity {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let f: IdentityFile = serde_json::from_slice(&bytes).context("parsing identity JSON")?;
        let sk_bytes: [u8; 32] = hex::decode(f.ed_sk.trim())?
            .try_into()
            .map_err(|_| anyhow::anyhow!("ed_sk must be 32 bytes"))?;
        let device_irk: [u8; 16] = if f.dirk.is_empty() {
            [0u8; 16]
        } else {
            hex::decode(f.dirk.trim())?.try_into().map_err(|_| anyhow::anyhow!("dirk must be 16 bytes"))?
        };
        let mut peers = Vec::new();
        for p in f.peers {
            let pk: [u8; 32] = hex::decode(p.edpk.trim())?
                .try_into()
                .map_err(|_| anyhow::anyhow!("peer edpk must be 32 bytes"))?;
            peers.push((p.label, VerifyingKey::from_bytes(&pk)?));
        }
        Ok(PairingIdentity { signing: SigningKey::from_bytes(&sk_bytes), device_irk, peers })
    }
}

// ---------------------------------------------------------------------------
// Pair-Verify client state machine (M1..M4)
// ---------------------------------------------------------------------------

fn pair_verify_key(shared_secret: &[u8]) -> [u8; KEY_LEN] {
    let k = hkdf_sha512(shared_secret, b"Pair-Verify-Encrypt-Salt", b"Pair-Verify-Encrypt-Info", KEY_LEN);
    let mut out = [0u8; KEY_LEN];
    out.copy_from_slice(&k);
    out
}

/// ChaCha20-Poly1305 as Pair-Verify uses it. TODO(validate): the reference
/// invoked a CoreCrypto "64x64" primitive with an 8-byte nonce; here we use the
/// RFC 8439 nonce `0x00000000 || label` (HAP convention). Unverified against a
/// real device.
fn pv_nonce(label: &[u8; 8]) -> Nonce {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(label);
    *Nonce::from_slice(&n)
}

pub struct PairVerifyClient {
    ephemeral: Option<EphemeralSecret>,
    public: PublicKey,
    identity: PairingIdentity,
    shared: Option<[u8; 32]>,
}

impl PairVerifyClient {
    pub fn new(identity: PairingIdentity) -> Self {
        let ephemeral = EphemeralSecret::random_from_rng(rand::rngs::OsRng);
        let public = PublicKey::from(&ephemeral);
        PairVerifyClient { ephemeral: Some(ephemeral), public, identity, shared: None }
    }

    /// M1: our public key + state=1 + appFlags=1, wrapped OPACK("_pd": TLV).
    pub fn build_m1(&self) -> ContinuityPacket {
        let mut tlv = Tlv8::new();
        tlv.push(tlv_type::PUBLIC_KEY, self.public.as_bytes().to_vec())
            .push_u8(tlv_type::STATE, 1)
            .push_u8(tlv_type::APP_FLAGS, 1);
        ContinuityPacket::new(PacketType::PairVerifyPublicKey, wrap_pairing_data(&tlv))
    }

    /// Process the peer's M2 (their public key + encrypted signature), verify
    /// the peer, and produce M3 (our encrypted signature + state=3).
    pub fn process_m2(&mut self, m2: &ContinuityPacket) -> Result<ContinuityPacket> {
        let tlv = m2.pairing_tlv()?;
        let peer_pub_bytes: [u8; 32] = tlv
            .get(tlv_type::PUBLIC_KEY)
            .context("M2 missing peer public key")?
            .try_into()
            .map_err(|_| anyhow::anyhow!("peer public key not 32 bytes"))?;
        let peer_pub = PublicKey::from(peer_pub_bytes);

        let eph = self.ephemeral.take().context("M1 not sent")?;
        let shared = eph.diffie_hellman(&peer_pub).to_bytes();
        self.shared = Some(shared);

        let key = pair_verify_key(&shared);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));

        // Decrypt the peer's signature blob (a TLV containing SIGNATURE).
        let enc = tlv.get(tlv_type::ENCRYPTED_DATA).context("M2 missing encrypted data")?;
        let dec = cipher
            .decrypt(&pv_nonce(b"PV-Msg02"), enc)
            .map_err(|_| anyhow::anyhow!("PV-Msg02 decrypt failed (key or nonce mismatch — see TODO)"))?;
        let inner = Tlv8::decode(&dec);
        let sig_bytes = inner.get(tlv_type::SIGNATURE).context("no signature in M2")?;
        let signature = Signature::from_slice(sig_bytes).context("bad signature length")?;

        // Peer signs peer_pub || our_pub.
        let mut signed = Vec::with_capacity(64);
        signed.extend_from_slice(&peer_pub_bytes);
        signed.extend_from_slice(self.public.as_bytes());
        let verified = self
            .identity
            .peers
            .iter()
            .find(|(_, vk)| vk.verify(&signed, &signature).is_ok())
            .map(|(label, _)| label.clone());
        if let Some(label) = &verified {
            tracing::info!(peer = %label, "Pair-Verify: peer signature verified");
        } else {
            // The reference also proceeds on verify failure (logs only); we do
            // too, but say so loudly.
            tracing::warn!("Pair-Verify: no known peer matched the signature");
        }

        // M3: sign our_pub || peer_pub, encrypt, send with state=3.
        let mut our_signed = Vec::with_capacity(64);
        our_signed.extend_from_slice(self.public.as_bytes());
        our_signed.extend_from_slice(&peer_pub_bytes);
        let our_sig = self.identity.signing.sign(&our_signed);
        let mut inner_tlv = Tlv8::new();
        inner_tlv.push(tlv_type::SIGNATURE, our_sig.to_bytes().to_vec());
        let sealed = cipher
            .encrypt(&pv_nonce(b"PV-Msg03"), inner_tlv.encode().as_slice())
            .map_err(|_| anyhow::anyhow!("PV-Msg03 encrypt failed"))?;

        let mut out = Tlv8::new();
        out.push(tlv_type::ENCRYPTED_DATA, sealed).push_u8(tlv_type::STATE, 3);
        Ok(ContinuityPacket::new(PacketType::PairVerifyContinue, wrap_pairing_data(&out)))
    }

    /// M4: peer confirms with state=4 and nothing else.
    pub fn check_m4(&self, m4: &ContinuityPacket) -> Result<ContentChannel> {
        let tlv = m4.pairing_tlv()?;
        if tlv.get(tlv_type::STATE) != Some(&[4]) {
            bail!("Pair-Verify M4 did not report state=4");
        }
        let shared = self.shared.context("no shared secret; process_m2 not run")?;
        Ok(ContentChannel::from_shared_secret(&shared, Role::Client))
    }

    #[allow(dead_code)]
    pub fn identity_id_type() -> u8 {
        tlv_type::IDENTITY_ID
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hkdf_is_deterministic_and_sized() {
        let a = hkdf_sha512(b"secret", b"salt", b"info", 32);
        let b = hkdf_sha512(b"secret", b"salt", b"info", 32);
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
        assert_ne!(a, hkdf_sha512(b"secret", b"salt", b"other", 32));
    }

    #[test]
    fn content_channel_roundtrip_and_direction() {
        // Client and server derive mirrored keys from the same secret.
        let secret = [7u8; 32];
        let mut client = ContentChannel::from_shared_secret(&secret, Role::Client);
        let mut server = ContentChannel::from_shared_secret(&secret, Role::Server);

        let ct = client.encrypt(b"hello", b"").unwrap();
        assert_eq!(server.decrypt(&ct, b"").unwrap(), b"hello");
        // Nonces advanced in lock-step.
        let ct2 = client.encrypt(b"world", b"").unwrap();
        assert_eq!(server.decrypt(&ct2, b"").unwrap(), b"world");
        // Reverse direction.
        let sc = server.encrypt(b"back", b"").unwrap();
        assert_eq!(client.decrypt(&sc, b"").unwrap(), b"back");
    }

    #[test]
    fn ecdh_agreement() {
        let a = EphemeralSecret::random_from_rng(rand::rngs::OsRng);
        let a_pub = PublicKey::from(&a);
        let b = EphemeralSecret::random_from_rng(rand::rngs::OsRng);
        let b_pub = PublicKey::from(&b);
        assert_eq!(a.diffie_hellman(&b_pub).to_bytes(), b.diffie_hellman(&a_pub).to_bytes());
    }

    #[test]
    fn ed25519_verify_over_concatenated_pubkeys() {
        use rand::rngs::OsRng;
        let sk = SigningKey::generate(&mut OsRng);
        let vk = sk.verifying_key();
        let mut msg = vec![1u8; 32];
        msg.extend_from_slice(&[2u8; 32]);
        let sig = sk.sign(&msg);
        assert!(vk.verify(&msg, &sig).is_ok());
    }

    #[test]
    fn continuity_packet_roundtrip() {
        let body = opack::encode(&Value::dict([("_pd", Value::Bytes(vec![0x06, 0x01, 0x04]))]));
        let pkt = ContinuityPacket::new(PacketType::PairVerifyPublicKey, body.clone());
        let wire = pkt.serialize();
        assert_eq!(wire[0], 0x05);
        assert_eq!(u16::from_be_bytes([wire[2], wire[3]]) as usize, body.len());
        let parsed = ContinuityPacket::parse(&wire).unwrap();
        assert_eq!(parsed.ptype, PacketType::PairVerifyPublicKey);
        assert_eq!(parsed.body, body);
    }

    #[test]
    fn encrypted_packet_len_includes_tag() {
        let pkt = ContinuityPacket::new(PacketType::EncryptedData, vec![0u8; 10]);
        let wire = pkt.serialize();
        // Advertised length = body + 16 (Poly1305 tag).
        assert_eq!(u16::from_be_bytes([wire[2], wire[3]]), 26);
    }

    /// Exercise the M1->M3 framing end-to-end with a locally-built peer so the
    /// TLV/OPACK/ChaCha wiring is covered even without real keys. (This is NOT
    /// a real Pair-Verify against a device.)
    #[test]
    fn pair_verify_local_loopback() {
        use rand::rngs::OsRng;

        // Peer identity.
        let peer_sk = SigningKey::generate(&mut OsRng);
        let peer_vk = peer_sk.verifying_key();

        // Our identity knows the peer's edPK.
        let our_sk = SigningKey::generate(&mut OsRng);
        let identity = PairingIdentity {
            signing: our_sk,
            device_irk: [0u8; 16],
            peers: vec![("peer".into(), peer_vk)],
        };

        let mut client = PairVerifyClient::new(identity);
        let m1 = client.build_m1();
        // Client's ephemeral public from M1.
        let m1_tlv = m1.pairing_tlv().unwrap();
        let client_pub: [u8; 32] = m1_tlv.get(tlv_type::PUBLIC_KEY).unwrap().try_into().unwrap();

        // Build a peer M2: peer ephemeral, ECDH, sign peer_pub||client_pub.
        let peer_eph = EphemeralSecret::random_from_rng(OsRng);
        let peer_pub = PublicKey::from(&peer_eph);
        let shared = peer_eph.diffie_hellman(&PublicKey::from(client_pub)).to_bytes();
        let key = pair_verify_key(&shared);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
        let mut signed = peer_pub.as_bytes().to_vec();
        signed.extend_from_slice(&client_pub);
        let sig = peer_sk.sign(&signed);
        let mut inner = Tlv8::new();
        inner.push(tlv_type::SIGNATURE, sig.to_bytes().to_vec());
        let sealed = cipher.encrypt(&pv_nonce(b"PV-Msg02"), inner.encode().as_slice()).unwrap();
        let mut m2_tlv = Tlv8::new();
        m2_tlv
            .push(tlv_type::PUBLIC_KEY, peer_pub.as_bytes().to_vec())
            .push(tlv_type::ENCRYPTED_DATA, sealed)
            .push_u8(tlv_type::STATE, 2);
        let m2 = ContinuityPacket::new(PacketType::PairVerifyContinue, wrap_pairing_data(&m2_tlv));

        // Client processes M2 -> M3; verify M3 decrypts and carries our sig.
        let m3 = client.process_m2(&m2).expect("process M2");
        let m3_tlv = m3.pairing_tlv().unwrap();
        assert_eq!(m3_tlv.get(tlv_type::STATE), Some(&[3][..]));
        let enc = m3_tlv.get(tlv_type::ENCRYPTED_DATA).unwrap();
        let dec = cipher.decrypt(&pv_nonce(b"PV-Msg03"), enc).unwrap();
        assert!(Tlv8::decode(&dec).get(tlv_type::SIGNATURE).is_some());

        // M4 -> content channel.
        let mut m4_tlv = Tlv8::new();
        m4_tlv.push_u8(tlv_type::STATE, 4);
        let m4 = ContinuityPacket::new(PacketType::PairVerifyContinue, wrap_pairing_data(&m4_tlv));
        assert!(client.check_m4(&m4).is_ok());
    }
}
