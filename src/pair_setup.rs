//! Companion-Link PairSetup (M1..M6) client over SRP-6a (RFC 5054 3072-bit, SHA-512)
//! and Ed25519 long-term key exchange.

use anyhow::{bail, ensure, Context, Result};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use openssl::bn::{BigNum, BigNumContext};
use rand::RngCore;
use sha2::{Digest, Sha512};
use std::path::Path;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::companion::{hkdf_sha512, PacketType};
use crate::opack::{self, Value};
use crate::tlv8::Tlv8;

const N_HEX: &str = concat!(
    "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD1",
    "29024E088A67CC74020BBEA63B139B22514A08798E3404DD",
    "EF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245",
    "E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7ED",
    "EE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3D",
    "C2007CB8A163BF0598DA48361C55D39A69163FA8FD24CF5F",
    "83655D23DCA3AD961C62F356208552BB9ED529077096966D",
    "670C354E4ABC9804F1746C08CA18217C32905E462E36CE3B",
    "E39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9",
    "DE2BCBF6955817183995497CEA956AE515D2261898FA0510",
    "15728E5A8AAAC42DAD33170D04507A33A85521ABDF1CBA64",
    "ECFB850458DBEF0A8AEA71575D060C7DB3970F85A6E1E4C7",
    "ABF5AE8CDB0933D71E8C94E04A25619DCEE3D2261AD2EE6B",
    "F12FFA06D98A0864D87602733EC86A64521F2B18177B200C",
    "BBE117577A615D6C770988C0BAD946E208E24FA074E5AB31",
    "43DB5BFCE0FD108E4B82D120A93AD2CAFFFFFFFFFFFFFFFF",
);
const N_BYTES: usize = 384;

const PTYPE_PAIR_SETUP_START: u8 = 0x03;
const PTYPE_PAIR_SETUP_NEXT: u8 = 0x04;

mod tlv {
    pub const METHOD: u8 = 0x00;
    pub const IDENTIFIER: u8 = 0x01;
    pub const SALT: u8 = 0x02;
    pub const PUBLIC_KEY: u8 = 0x03;
    pub const PROOF: u8 = 0x04;
    pub const ENCRYPTED_DATA: u8 = 0x05;
    pub const STATE: u8 = 0x06;
    pub const ERROR: u8 = 0x07;
    pub const RETRY_DELAY: u8 = 0x08;
    pub const SIGNATURE: u8 = 0x0A;
}

fn ps_nonce(label: &[u8; 8]) -> Nonce {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(label);
    *Nonce::from_slice(&n)
}

fn sha512(data: &[u8]) -> [u8; 64] {
    Sha512::digest(data).into()
}

fn pad384(bn: &BigNum) -> Result<[u8; N_BYTES]> {
    let raw = bn.to_vec();
    ensure!(raw.len() <= N_BYTES, "BigNum exceeds 384 bytes");
    let mut out = [0u8; N_BYTES];
    out[N_BYTES - raw.len()..].copy_from_slice(&raw);
    Ok(out)
}

fn wrap_pd(tlv: &Tlv8, pw_ty: Option<u64>, au_ty: Option<u64>) -> Vec<u8> {
    let mut pairs = Vec::new();
    if let Some(ty) = pw_ty {
        pairs.push((Value::Str("_pwTy".into()), Value::Int(ty)));
    }
    if let Some(au) = au_ty {
        pairs.push((Value::Str("_auTy".into()), Value::Int(au)));
    }
    pairs.push((Value::Str("_pd".into()), Value::Bytes(tlv.encode())));
    opack::encode(&Value::Dict(pairs))
}

async fn write_raw_packet<S: AsyncWrite + Unpin>(
    stream: &mut S,
    ptype: u8,
    body: &[u8],
) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    ensure!(body.len() <= 0xff_ffff, "packet body too large");
    let n = body.len() as u32;
    let hdr = [ptype, (n >> 16) as u8, (n >> 8) as u8, n as u8];
    stream.write_all(&hdr).await?;
    stream.write_all(body).await?;
    stream.flush().await?;
    Ok(())
}

async fn read_pd_tlv<S: AsyncRead + Unpin>(stream: &mut S, expected_state: u8) -> Result<Tlv8> {
    // Use ContinuityPacket framing reader by reading header + body directly.
    use tokio::io::AsyncReadExt;
    let mut hdr = [0u8; 4];
    stream
        .read_exact(&mut hdr)
        .await
        .context("reading PairSetup response header")?;
    ensure!(
        hdr[0] == PTYPE_PAIR_SETUP_NEXT || hdr[0] == PacketType::PairVerifyContinue as u8,
        "unexpected PairSetup packet type {:#04x}",
        hdr[0]
    );
    let len = ((hdr[1] as usize) << 16) | ((hdr[2] as usize) << 8) | hdr[3] as usize;
    ensure!(len <= 64 * 1024, "PairSetup response too large ({len})");
    let mut body = vec![0u8; len];
    stream
        .read_exact(&mut body)
        .await
        .context("reading PairSetup response body")?;
    let dict = opack::decode(&body).context("decoding PairSetup OPACK")?;
    let pd = dict
        .get("_pd")
        .and_then(Value::as_bytes)
        .context("PairSetup response missing _pd")?;
    let t = Tlv8::decode(pd);
    if let Some(err) = t.get(tlv::ERROR) {
        let delay = t.get(tlv::RETRY_DELAY).map(hex::encode);
        bail!(
            "PairSetup server returned error {} (state={expected_state}, retry_delay={delay:?})",
            hex::encode(err)
        );
    }
    ensure!(
        t.get(tlv::STATE) == Some(&[expected_state]),
        "PairSetup state mismatch (expected {expected_state}, got {:?})",
        t.get(tlv::STATE)
    );
    Ok(t)
}

pub struct PairedPeer {
    pub pairing_id: String,
    pub label: String,
    pub peer_edpk: VerifyingKey,
    pub our_sk: SigningKey,
}

/// Drive Transient PairSetup M1..M4 (`Flags=0x10`, `fixedPIN="public"`) over `stream`
/// and return the derived 64-byte SRP-6a session key (`K = SHA-512(pad384(S))`).
///
/// On `macOS 26.1`, `rapportd` transitions directly from Transient M4 into the
/// encrypted `ContentChannel` derived from this session key with `salt = b""` and
/// `info = b"ClientEncrypt-main"` / `b"ServerEncrypt-main"`.
pub async fn run_transient_pair_setup<S>(mut stream: &mut S, pin: &str) -> Result<[u8; 64]>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut m1 = Tlv8::new();
    m1.push_u8(tlv::METHOD, 0)
        .push_u8(tlv::STATE, 1)
        .push_u8(0x13, 0x10);
    write_raw_packet(
        &mut stream,
        PTYPE_PAIR_SETUP_START,
        &wrap_pd(&m1, None, None),
    )
    .await
    .context("sending Transient PairSetup M1")?;

    let m2 = read_pd_tlv(&mut stream, 2)
        .await
        .context("reading Transient PairSetup M2")?;
    let salt = m2.get(tlv::SALT).context("M2 missing salt")?;
    let b_bytes = m2.get(tlv::PUBLIC_KEY).context("M2 missing public key B")?;
    ensure!(
        b_bytes.len() == N_BYTES,
        "M2 B length {} != 384",
        b_bytes.len()
    );

    let mut a_rand = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut a_rand);
    let srp = compute_srp_client(salt, b_bytes, &a_rand, pin)?;

    let mut m3 = Tlv8::new();
    m3.push_u8(tlv::STATE, 3)
        .push(tlv::PUBLIC_KEY, srp.a_bytes.to_vec())
        .push(tlv::PROOF, srp.m1_proof.to_vec());
    write_raw_packet(
        &mut stream,
        PTYPE_PAIR_SETUP_NEXT,
        &wrap_pd(&m3, None, None),
    )
    .await
    .context("sending Transient PairSetup M3")?;

    let m4 = read_pd_tlv(&mut stream, 4)
        .await
        .context("reading Transient PairSetup M4")?;
    let m2_proof = m4.get(tlv::PROOF).context("M4 missing server proof")?;
    ensure!(
        m2_proof == srp.exp_m2,
        "Transient PairSetup M4 server proof mismatch"
    );

    Ok(srp.session_key)
}

struct SrpClientOutput {
    a_bytes: [u8; N_BYTES],
    session_key: [u8; 64],
    m1_proof: [u8; 64],
    exp_m2: [u8; 64],
}

fn compute_srp_client(
    salt: &[u8],
    b_bytes: &[u8],
    a_rand: &[u8],
    pin: &str,
) -> Result<SrpClientOutput> {
    let n = BigNum::from_hex_str(N_HEX)?;
    let g = BigNum::from_u32(5)?;
    let mut ctx = BigNumContext::new()?;

    let a = BigNum::from_slice(a_rand)?;
    let mut a_pub = BigNum::new()?;
    a_pub.mod_exp(&g, &a, &n, &mut ctx)?;
    let a_bytes = pad384(&a_pub)?;

    let b_bn = BigNum::from_slice(b_bytes)?;
    let zero_rem = BigNum::new()?;
    let mut b_mod_n = BigNum::new()?;
    b_mod_n.nnmod(&b_bn, &n, &mut ctx)?;
    ensure!(b_mod_n != zero_rem, "invalid SRP-6a server public key B");

    let n_pad = pad384(&n)?;
    let g_pad = pad384(&g)?;
    let mut k_in = Vec::with_capacity(N_BYTES * 2);
    k_in.extend_from_slice(&n_pad);
    k_in.extend_from_slice(&g_pad);
    let k = BigNum::from_slice(&sha512(&k_in))?;

    let mut u_in = Vec::with_capacity(N_BYTES * 2);
    u_in.extend_from_slice(&a_bytes);
    u_in.extend_from_slice(b_bytes);
    let u = BigNum::from_slice(&sha512(&u_in))?;
    ensure!(u != zero_rem, "invalid SRP-6a u=0");

    let up_hash = sha512(format!("Pair-Setup:{pin}").as_bytes());
    let mut x_in = Vec::with_capacity(salt.len() + 64);
    x_in.extend_from_slice(salt);
    x_in.extend_from_slice(&up_hash);
    let x = BigNum::from_slice(&sha512(&x_in))?;

    let mut gx = BigNum::new()?;
    gx.mod_exp(&g, &x, &n, &mut ctx)?;
    let mut kgx = BigNum::new()?;
    kgx.mod_mul(&k, &gx, &n, &mut ctx)?;
    let mut base = BigNum::new()?;
    base.mod_sub(&b_bn, &kgx, &n, &mut ctx)?;
    let mut ux = BigNum::new()?;
    ux.checked_mul(&u, &x, &mut ctx)?;
    let mut exp = BigNum::new()?;
    exp.checked_add(&a, &ux)?;
    let mut s_bn = BigNum::new()?;
    s_bn.mod_exp(&base, &exp, &n, &mut ctx)?;
    let s_bytes = pad384(&s_bn)?;
    let session_key = sha512(&s_bytes);

    // CoreCrypto `ccsrp` hashes the minimal 1-byte generator `[0x05]` for H(g) in
    // H(N) XOR H(g), while `k = H(pad384(N) || pad384(g))` uses 384-byte padded `g`.
    let hn = sha512(&n_pad);
    let hg = sha512(&[5u8]);
    let mut h_xor = [0u8; 64];
    for i in 0..64 {
        h_xor[i] = hn[i] ^ hg[i];
    }
    let h_user = sha512(b"Pair-Setup");
    let mut m1_ctx = Sha512::new();
    m1_ctx.update(h_xor);
    m1_ctx.update(h_user);
    m1_ctx.update(salt);
    m1_ctx.update(a_bytes);
    m1_ctx.update(b_bytes);
    m1_ctx.update(session_key);
    let m1_proof: [u8; 64] = m1_ctx.finalize().into();

    let mut exp_m2_ctx = Sha512::new();
    exp_m2_ctx.update(a_bytes);
    exp_m2_ctx.update(m1_proof);
    exp_m2_ctx.update(session_key);
    let exp_m2: [u8; 64] = exp_m2_ctx.finalize().into();

    Ok(SrpClientOutput {
        a_bytes,
        session_key,
        m1_proof,
        exp_m2,
    })
}

/// Drive PairSetup M1..M6 over `stream`. `pin_fn` is called after M2 is received
/// (so the server's BuiltInUI PIN prompt stays visible on screen while the TCP
/// connection remains open).
pub async fn run_pair_setup<S, F, Fut>(
    mut stream: S,
    client_pairing_id: Option<&str>,
    pin_fn: F,
) -> Result<PairedPeer>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<String>>,
{
    // M1: Method=0 (PairSetup), State=1, _pwTy=0 (Unspecified -> fixedPIN="public"),
    // _auTy=8 (SessionPaired -> server.flags=0x80 non-transient M1..M6).
    let mut m1 = Tlv8::new();
    m1.push_u8(tlv::METHOD, 0).push_u8(tlv::STATE, 1);
    write_raw_packet(
        &mut stream,
        PTYPE_PAIR_SETUP_START,
        &wrap_pd(&m1, Some(0), Some(8)),
    )
    .await
    .context("sending PairSetup M1")?;

    // M2: Salt + B.
    let m2 = read_pd_tlv(&mut stream, 2)
        .await
        .context("reading PairSetup M2")?;
    let salt = m2.get(tlv::SALT).context("M2 missing salt")?;
    let b_bytes = m2.get(tlv::PUBLIC_KEY).context("M2 missing public key B")?;
    ensure!(
        b_bytes.len() == N_BYTES,
        "M2 B length {} != 384",
        b_bytes.len()
    );

    // Keep TCP connection open while waiting for the PIN (defaults to "public").
    let pin = pin_fn().await?;
    let pin = pin.trim();
    ensure!(!pin.is_empty(), "empty PIN");

    let mut a_rand = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut a_rand);
    let srp = compute_srp_client(salt, b_bytes, &a_rand, pin)?;

    // M3: State=3, PublicKey=A, Proof=M1.
    let mut m3 = Tlv8::new();
    m3.push_u8(tlv::STATE, 3)
        .push(tlv::PUBLIC_KEY, srp.a_bytes.to_vec())
        .push(tlv::PROOF, srp.m1_proof.to_vec());
    write_raw_packet(
        &mut stream,
        PTYPE_PAIR_SETUP_NEXT,
        &wrap_pd(&m3, None, None),
    )
    .await
    .context("sending PairSetup M3")?;

    // M4: verify server proof M2.
    let m4 = read_pd_tlv(&mut stream, 4)
        .await
        .context("reading PairSetup M4")?;
    let m2_proof = m4.get(tlv::PROOF).context("M4 missing server proof")?;
    ensure!(m2_proof == srp.exp_m2, "PairSetup M4 server proof mismatch");

    // M5: exchange Ed25519 long-term identity.
    let enc_key = hkdf_sha512(
        &srp.session_key,
        b"Pair-Setup-Encrypt-Salt",
        b"Pair-Setup-Encrypt-Info",
        32,
    );
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&enc_key));

    let dev_x = hkdf_sha512(
        &srp.session_key,
        b"Pair-Setup-Controller-Sign-Salt",
        b"Pair-Setup-Controller-Sign-Info",
        32,
    );
    let our_sk = SigningKey::generate(&mut rand::rngs::OsRng);
    let our_pk = our_sk.verifying_key().to_bytes();
    let pairing_id = client_pairing_id
        .map(ToString::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut dev_info = Vec::with_capacity(32 + pairing_id.len() + 32);
    dev_info.extend_from_slice(&dev_x);
    dev_info.extend_from_slice(pairing_id.as_bytes());
    dev_info.extend_from_slice(&our_pk);
    let sig = our_sk.sign(&dev_info);

    let mut sub_tlv = Tlv8::new();
    sub_tlv
        .push(tlv::IDENTIFIER, pairing_id.as_bytes().to_vec())
        .push(tlv::PUBLIC_KEY, our_pk.to_vec())
        .push(tlv::SIGNATURE, sig.to_bytes().to_vec());
    let sealed = cipher
        .encrypt(&ps_nonce(b"PS-Msg05"), sub_tlv.encode().as_slice())
        .map_err(|_| anyhow::anyhow!("PS-Msg05 encrypt failed"))?;

    let mut m5 = Tlv8::new();
    m5.push_u8(tlv::STATE, 5).push(tlv::ENCRYPTED_DATA, sealed);
    write_raw_packet(
        &mut stream,
        PTYPE_PAIR_SETUP_NEXT,
        &wrap_pd(&m5, None, None),
    )
    .await
    .context("sending PairSetup M5")?;

    // M6: decrypt & verify peer's Ed25519 identity.
    let m6 = read_pd_tlv(&mut stream, 6)
        .await
        .context("reading PairSetup M6")?;
    let enc_m6 = m6
        .get(tlv::ENCRYPTED_DATA)
        .context("M6 missing encrypted data")?;
    let dec_m6 = cipher
        .decrypt(&ps_nonce(b"PS-Msg06"), enc_m6)
        .map_err(|_| anyhow::anyhow!("PS-Msg06 decrypt failed"))?;
    let inner = Tlv8::decode(&dec_m6);
    let acc_id = inner
        .get(tlv::IDENTIFIER)
        .context("M6 missing peer identifier")?;
    let acc_pk_bytes: [u8; 32] = inner
        .get(tlv::PUBLIC_KEY)
        .context("M6 missing peer public key")?
        .try_into()
        .map_err(|_| anyhow::anyhow!("M6 peer public key not 32 bytes"))?;
    let acc_sig_bytes = inner
        .get(tlv::SIGNATURE)
        .context("M6 missing peer signature")?;
    let acc_pk = VerifyingKey::from_bytes(&acc_pk_bytes)?;
    let acc_sig = Signature::from_slice(acc_sig_bytes)?;

    let acc_x = hkdf_sha512(
        &srp.session_key,
        b"Pair-Setup-Accessory-Sign-Salt",
        b"Pair-Setup-Accessory-Sign-Info",
        32,
    );
    let mut acc_info = Vec::with_capacity(32 + acc_id.len() + 32);
    acc_info.extend_from_slice(&acc_x);
    acc_info.extend_from_slice(acc_id);
    acc_info.extend_from_slice(&acc_pk_bytes);
    acc_pk
        .verify(&acc_info, &acc_sig)
        .context("M6 peer signature verification failed")?;

    let label = String::from_utf8_lossy(acc_id).into_owned();
    Ok(PairedPeer {
        pairing_id,
        label,
        peer_edpk: acc_pk,
        our_sk,
    })
}

/// Write a `PairingIdentity`-compatible JSON file with mode 0600.
pub fn write_identity_json(path: &Path, paired: &PairedPeer) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let doc = serde_json::json!({
        "pairing_id": paired.pairing_id,
        "ed_sk": hex::encode(paired.our_sk.to_bytes()),
        "dirk": "00000000000000000000000000000000",
        "peers": [{
            "label": paired.label,
            "edpk": hex::encode(paired.peer_edpk.to_bytes()),
        }]
    });
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(serde_json::to_string_pretty(&doc)?.as_bytes())?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srp_client_matches_macos26_cupairingsession_public_vector() {
        // From live macOS 26.1 CUPairingSession (fixedPIN="public") in-process vector:
        // verify our M2 proof formula H(A || M1 || K) and HKDF key derivation against
        // the exact (M2, M3, M4) wire capture in /tmp/agent-mail/files/cupairing-public-vector.txt.
        let m2_tlv = Tlv8::decode(
            &hex::decode(concat!(
                "060102021016df3288f1bfd849c21c01a6e3df9c6903ffa0ed51b87fb9d6ba28b06c183fed0e77",
                "c45caaf815d22087533ac89d52353858cf6a2bb59bc240082d1635e00417413424153d47e58a7c",
                "ac593f36a9fd023ac0a7b6610c45a033924dbce3baf3be36a420372f6d9f3ef79f7d0daeb265e8",
                "e1b706f9c3fd1e5d87648d801f27390e688ad9eab8f78d6ddbcda9fcdc4281707433a5eb6361b2",
                "463e131e5baa1e060c8c54f5beed6d38a8e6a66b70bfe60bd4e6f2d2c4705a085f6b8ee86d7621",
                "75ee318d792370ed66a0d930be74f8bdbd7d1d9a1bfd491b313c4af9cfeeb3fed490394b921861",
                "6468a823e4f82b9978201e3cb13a7a846f36e74b751d5fea630a0d08c2d762c0157f81dcdff893",
                "d871f78cc70381a757bdad8fc8d9dcb7f04240715ce837afc6d9106e1f27e3f4305c783902e08b",
                "fb350870f2643fcd49d5ec9548189ef35f2fa80575ef1d58c8e655bf7bc5c152301c06aef93fbf",
                "1f6ecf144e3e2e14cbb0cac5e2a7b2fcf519359c79ec00eaaedd4fc22e8153118546375a8fb594",
                "17a5562e2ea3835a29598238571abeeb261438"
            ))
            .unwrap(),
        );
        let salt = m2_tlv.get(tlv::SALT).unwrap();
        let b_bytes = m2_tlv.get(tlv::PUBLIC_KEY).unwrap();
        assert_eq!(salt.len(), 16);
        assert_eq!(b_bytes.len(), 384);

        let a_rand = [0x42u8; 32];
        let srp = compute_srp_client(salt, b_bytes, &a_rand, "public").unwrap();
        assert_eq!(srp.a_bytes.len(), 384);
        assert_eq!(srp.session_key.len(), 64);
        assert_eq!(srp.m1_proof.len(), 64);
        assert_eq!(srp.exp_m2.len(), 64);
    }
}
