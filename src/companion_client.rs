//! Milestone 2: the transport-INDEPENDENT companion-link client that pulls
//! Universal Clipboard content from a same-Apple-ID peer.
//!
//! This module is the socket driver the M2 plan (`docs/architecture.md` §4.2)
//! calls for. It sits on top of the pieces already in `companion.rs`
//! (`PairVerifyClient`, `ContinuityPacket` framing, `ContentChannel`,
//! `PairingIdentity`) and adds:
//!
//!   * a tokio TCP client that opens the socket and streams length-prefixed
//!     `ContinuityPacket`s,
//!   * the Pair-Verify M1->M4 drive loop, ending in an encrypted [`Session`],
//!   * on the session, the payload-transfer exchanges from Stute et al. (USENIX
//!     2021): the P1/P2 system-info exchange and the P3/P4 pasteboard fetch.
//!
//! ## What is validated vs. reconstructed
//!
//! **Loopback-validated** (exercised end-to-end by the tests below against an
//! in-process [`run_mock_server`], no network, no hardware):
//!   * the stream framing of `ContinuityPacket`s (header + body, and the
//!     EncryptedData +16 tag convention round-tripping with `serialize`),
//!   * the Pair-Verify handshake wiring (M1->M4) and key derivation,
//!   * the OPACK request/response round-trip for system-info and pasteboard.
//!
//! **UNVALIDATED until tested against a real device** (best reconstruction from
//! the paper + OPACK structure; every guessy field is isolated in the `uc`
//! module below and marked): the *exact* OPACK dict shape of the Universal
//! Clipboard system-info and pasteboard request/response — key names, the
//! request selector, the response envelope, and whether content packets carry
//! any AAD. These are our reconstruction, not confirmed wire captures.
//!
//! The TLS long-payload path for items >10 KB is deliberately NOT implemented
//! yet — see the TODO in [`Session::fetch_pasteboard`].

#![allow(dead_code)] // M2 scaffolding: exercised by unit tests; wired into the runtime once AWDL + macOS keys exist.

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::companion::{
    ContentChannel, ContinuityPacket, PacketType, PairVerifyClient, PairVerifyServer,
    PairingIdentity,
};
use crate::opack::{self, Value};

/// Sanity cap on a single framed body, so a bad/hostile length prefix cannot
/// make us allocate unbounded memory. Comfortably above any inline pasteboard.
const MAX_PACKET_BODY: usize = 4 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Stream framing: length-prefixed ContinuityPackets over an async byte stream
// ---------------------------------------------------------------------------

/// Write one `ContinuityPacket` (`type|len_be24|body`) and flush.
pub(crate) async fn write_packet<S: AsyncWrite + Unpin>(
    stream: &mut S,
    pkt: &ContinuityPacket,
) -> Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        stream
            .write_all(&pkt.serialize()?)
            .await
            .context("writing continuity packet")?;
        stream.flush().await.context("flushing continuity packet")?;
        anyhow::Ok(())
    })
    .await
    .context("continuity write timed out")?
}

/// Read exactly the 24-bit wire body length, including the AEAD tag.
pub(crate) async fn read_packet<S: AsyncRead + Unpin>(stream: &mut S) -> Result<ContinuityPacket> {
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        read_packet_inner(stream),
    )
    .await
    .context("continuity read timed out")?
}
async fn read_packet_inner<S: AsyncRead + Unpin>(stream: &mut S) -> Result<ContinuityPacket> {
    let mut header = [0u8; 4];
    stream
        .read_exact(&mut header)
        .await
        .context("reading continuity packet header")?;
    let ptype = PacketType::from_u8(header[0])
        .with_context(|| format!("unknown packet type {:#04x}", header[0]))?;
    let body_len = ((header[1] as usize) << 16) | ((header[2] as usize) << 8) | header[3] as usize;
    if ptype == PacketType::EncryptedData && body_len < 16 {
        bail!("encrypted frame is shorter than its tag");
    }
    if body_len > MAX_PACKET_BODY {
        bail!("continuity packet body of {body_len} bytes exceeds {MAX_PACKET_BODY} cap");
    }
    let mut body = vec![0u8; body_len];
    stream
        .read_exact(&mut body)
        .await
        .context("reading continuity packet body")?;
    Ok(ContinuityPacket { ptype, body })
}

// ---------------------------------------------------------------------------
// Universal Clipboard payloads (RECONSTRUCTED — see module docs)
// ---------------------------------------------------------------------------

/// Everything in here is our best reconstruction of the Universal Clipboard
/// OPACK payloads from Stute et al. and the general companion-link payload-
/// transfer shape. Isolated in one module so the guessy field names are easy to
/// find and swap once we have a real packet capture.
pub mod uc {
    use super::*;

    // OPACK envelope keys (from seemoo-lab/handoff-authentication-swift & rapportd).
    pub(super) const K_TYPE: &str = "_t"; // message type: 1=event, 2=request, 3=response
    pub(super) const K_ID: &str = "_i"; // request selector / name
    pub(super) const K_XID: &str = "_x"; // transaction id, echoed in the response
    pub(super) const K_CONTENT: &str = "_c"; // request/response content dictionary
    pub(super) const K_ERR_MSG: &str = "_em"; // remote error message
    pub(super) const K_ERR_CODE: &str = "_ec"; // remote error code

    pub(super) const MSG_EVENT: u64 = 1;
    pub(super) const MSG_REQUEST: u64 = 2;
    pub(super) const MSG_RESPONSE: u64 = 3;

    // Selectors (request names).
    pub(super) const SEL_SYSTEM_INFO: &str = "_systemInfo";
    pub(super) const SEL_PAYLOAD_REQUEST: &str = "com.apple.handoff.payload-request";

    pub(super) const CMD_PBTYPES: &str = "pbtypes";
    pub(super) const CMD_PBPASTE2: &str = "pbpaste2";

    /// A single pasteboard representation: a UTI type string plus its raw bytes.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct PasteboardItem {
        pub uti: String,
        pub data: Vec<u8>,
    }

    /// A pasteboard is an ordered list of representations of one copy.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct Pasteboard {
        pub items: Vec<PasteboardItem>,
    }

    impl Pasteboard {
        /// Convenience: the first `public.utf8-plain-text` (or `public.text`)
        /// representation decoded as UTF-8, if any.
        pub fn plain_text(&self) -> Option<String> {
            self.items
                .iter()
                .find(|it| it.uti == "public.utf8-plain-text" || it.uti == "public.text")
                .and_then(|it| String::from_utf8(it.data.clone()).ok())
        }
    }

    /// Peer/device system information exchanged in P1/P2. Only a few fields are
    /// modelled; unknown keys are ignored on parse.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct SystemInfo {
        pub name: String,
        pub model: String,
        pub os_version: String,
    }

    impl SystemInfo {
        const K_NAME: &'static str = "name";
        const K_MODEL: &'static str = "model";
        const K_OS: &'static str = "osVersion";

        pub(super) fn to_value(&self, msg_type: u64, xid: u64) -> Value {
            self.to_value_with_id(msg_type, xid, None)
        }

        pub(super) fn to_value_with_id(
            &self,
            msg_type: u64,
            xid: u64,
            pairing_id: Option<&str>,
        ) -> Value {
            let mut content_pairs = vec![
                (
                    Value::Str(Self::K_NAME.into()),
                    Value::Str(self.name.clone()),
                ),
                (
                    Value::Str(Self::K_MODEL.into()),
                    Value::Str(self.model.clone()),
                ),
                (
                    Value::Str(Self::K_OS.into()),
                    Value::Str(self.os_version.clone()),
                ),
            ];
            if let Some(id) = pairing_id {
                content_pairs.push((Value::Str("_i".into()), Value::Str(id.into())));
                content_pairs.push((Value::Str("_idsID".into()), Value::Str(id.into())));
            }
            let content = Value::Dict(content_pairs);
            let mut pairs = vec![
                (Value::Str(K_TYPE.into()), Value::Int(msg_type)),
                (Value::Str(K_XID.into()), Value::Int(xid)),
                (Value::Str(K_CONTENT.into()), content),
            ];
            if msg_type == MSG_REQUEST {
                pairs.push((Value::Str(K_ID.into()), Value::Str(SEL_SYSTEM_INFO.into())));
            }
            Value::Dict(pairs)
        }

        pub(super) fn from_value(v: &Value) -> SystemInfo {
            let src = v.get(K_CONTENT).unwrap_or(v);
            let s = |k: &str| {
                src.get(k)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            let os_version = src
                .get(Self::K_OS)
                .or_else(|| src.get("os"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            SystemInfo {
                name: s(Self::K_NAME),
                model: s(Self::K_MODEL),
                os_version,
            }
        }
    }

    /// Build a P3 `com.apple.handoff.payload-request` envelope (`pbtypes` or `pbpaste2`).
    pub(super) fn build_pasteboard_request(
        xid: u64,
        cmd: &str,
        identifier: &str,
        adv_payload: Option<&[u8]>,
    ) -> Value {
        let mut content_pairs = vec![
            (Value::Str("rClientCommand".into()), Value::Str(cmd.into())),
            (
                Value::Str("rIdentifier".into()),
                Value::Str(identifier.into()),
            ),
        ];
        if let Some(adv) = adv_payload {
            content_pairs.push((Value::Str("rAdvPayload".into()), Value::Bytes(adv.to_vec())));
        }
        Value::dict([
            (K_TYPE, Value::Int(MSG_REQUEST)),
            (K_ID, Value::Str(SEL_PAYLOAD_REQUEST.into())),
            (K_XID, Value::Int(xid)),
            (K_CONTENT, Value::Dict(content_pairs)),
        ])
    }

    /// Build a P4 pasteboard-fetch response using an `NSKeyedArchiver`
    /// `UASharedPasteboardInfoWrapper` inside `_c.rActPayload` (for loopback mock).
    pub(super) fn build_pasteboard_response(xid: u64, pb: &Pasteboard) -> Result<Value> {
        let archive = crate::pasteboard_archive::encode_wrapper_for_test(pb)?;
        Ok(Value::dict([
            (K_TYPE, Value::Int(MSG_RESPONSE)),
            (K_XID, Value::Int(xid)),
            (
                K_CONTENT,
                Value::dict([("rActPayload", Value::Bytes(archive))]),
            ),
        ]))
    }

    /// Parse a P4 pasteboard-fetch response (`_c.rActPayload`) into typed items.
    pub(super) fn parse_pasteboard_response(v: &Value) -> Result<Pasteboard> {
        let content = v
            .get(K_CONTENT)
            .context("pasteboard response missing `_c` content dict")?;
        let payload = content
            .get("rActPayload")
            .and_then(Value::as_bytes)
            .context("pasteboard response `_c` missing bytes `rActPayload`")?;
        crate::pasteboard_archive::decode_wrapper(payload)
            .context("decoding UASharedPasteboardInfoWrapper in rActPayload")
    }
}

pub use uc::{Pasteboard, PasteboardItem, SystemInfo};

// ---------------------------------------------------------------------------
// Encrypted OPACK exchange over a ContentChannel
// ---------------------------------------------------------------------------

// Content-phase packets are OPACK plaintext sealed with the post-handshake
// ContentChannel and framed as EncryptedData ContinuityPackets. The exact
// four-byte wire header is authenticated as AAD, as in the reference protocol.

async fn send_opack<S: AsyncWrite + Unpin>(
    stream: &mut S,
    channel: &mut ContentChannel,
    value: &Value,
) -> Result<()> {
    let plain = opack::encode(value);
    let header = ContinuityPacket::header(PacketType::EncryptedData, plain.len() + 16)?;
    let sealed = channel.encrypt(&plain, &header)?;
    write_packet(
        stream,
        &ContinuityPacket::new(PacketType::EncryptedData, sealed),
    )
    .await
}

async fn recv_opack<S: AsyncRead + Unpin>(
    stream: &mut S,
    channel: &mut ContentChannel,
) -> Result<Value> {
    let pkt = read_packet(stream).await?;
    if pkt.ptype != PacketType::EncryptedData {
        bail!(
            "expected EncryptedData in content phase, got {:?}",
            pkt.ptype
        );
    }
    let header = ContinuityPacket::header(pkt.ptype, pkt.body.len())?;
    let plain = channel.decrypt(&pkt.body, &header)?;
    opack::decode(&plain)
}

// ---------------------------------------------------------------------------
// Client session
// ---------------------------------------------------------------------------

/// An established, encrypted companion-link session: Pair-Verify has completed
/// and the [`ContentChannel`] is live. Generic over the byte stream so it can be
/// driven over a real `TcpStream` or an in-process loopback pipe in tests.
pub struct Session<S> {
    stream: S,
    channel: ContentChannel,
    xid: u64,
    peer_label: Option<String>,
    pairing_id: Option<String>,
}

const MAX_SKIPPED_EVENTS: usize = 16;

impl<S: AsyncRead + AsyncWrite + Unpin> Session<S> {
    /// Run Pair-Verify M1->M4 over an already-connected stream and return the
    /// encrypted session. Transport-independent: `stream` is any async byte
    /// stream (a TCP socket in production, a loopback socket in tests).
    pub async fn handshake(mut stream: S, identity: PairingIdentity) -> Result<Self> {
        let pairing_id = identity.pairing_id.clone();
        let mut client = PairVerifyClient::new(identity);
        write_packet(&mut stream, &client.build_m1())
            .await
            .context("stage M1: sending Pair-Verify M1")?;
        let m2 = read_packet(&mut stream)
            .await
            .context("stage M2: reading Pair-Verify M2")?;
        let m3 = client
            .process_m2(&m2)
            .context("stage M2: processing M2 / building M3")?;
        let peer_label = client.peer_label().map(ToString::to_string);
        write_packet(&mut stream, &m3)
            .await
            .context("stage M3: sending Pair-Verify M3")?;
        let m4 = read_packet(&mut stream)
            .await
            .context("stage M4: reading Pair-Verify M4")?;
        let channel = client
            .check_m4(&m4)
            .context("stage M4: verifying M4 / deriving content keys")?;
        Ok(Session {
            stream,
            channel,
            xid: 1,
            peer_label,
            pairing_id,
        })
    }

    /// Run Transient Pair-Setup M1->M4 (`Flags=0x10`, `fixedPIN="public"`) over
    /// `stream` and transition directly into an encrypted [`Session`] without
    /// requiring a long-term `RPIdentity` keypair.
    pub async fn handshake_transient(mut stream: S, pin: &str) -> Result<Self> {
        let session_key = crate::pair_setup::run_transient_pair_setup(&mut stream, pin)
            .await
            .context("stage Transient Pair-Setup M1..M4")?;
        let channel =
            ContentChannel::from_shared_secret(&session_key, crate::companion::Role::Client);
        Ok(Session {
            stream,
            channel,
            xid: 1,
            peer_label: Some("transient".into()),
            pairing_id: None,
        })
    }

    /// Authenticated peer label from Pair-Verify M2, if present.
    pub fn peer_label(&self) -> Option<&str> {
        self.peer_label.as_deref()
    }

    async fn recv_response(&mut self, expected_xid: u64, stage: &str) -> Result<Value> {
        for _ in 0..=MAX_SKIPPED_EVENTS {
            let msg = recv_opack(&mut self.stream, &mut self.channel)
                .await
                .with_context(|| format!("stage {stage}: reading OPACK message"))?;
            let msg_type = msg.get(uc::K_TYPE).and_then(Value::as_u64);
            if msg_type == Some(uc::MSG_EVENT) {
                tracing::debug!(stage, "skipping unrelated companion-link event (_t=1)");
                continue;
            }
            if msg_type != Some(uc::MSG_RESPONSE) {
                bail!("stage {stage}: expected response (_t=3), got _t={msg_type:?}");
            }
            let got_xid = msg.get(uc::K_XID).and_then(Value::as_u64);
            if got_xid != Some(expected_xid) {
                bail!(
                    "stage {stage}: transaction id mismatch (expected {expected_xid}, got {got_xid:?})"
                );
            }
            if let Some(em) = msg.get(uc::K_ERR_MSG).and_then(Value::as_str) {
                let ec = msg.get(uc::K_ERR_CODE).and_then(Value::as_u64);
                bail!("stage {stage}: remote error {ec:?}: {em}");
            }
            if let Some(ec) = msg.get(uc::K_ERR_CODE).and_then(Value::as_u64) {
                bail!("stage {stage}: remote error code {ec}");
            }
            return Ok(msg);
        }
        bail!("stage {stage}: exceeded {MAX_SKIPPED_EVENTS} skipped events waiting for _x={expected_xid}")
    }

    /// P1/P2 system-info exchange: send our system-info dict, read the peer's.
    pub async fn system_info_exchange(&mut self, ours: &SystemInfo) -> Result<SystemInfo> {
        let xid = self.next_xid();
        send_opack(
            &mut self.stream,
            &mut self.channel,
            &ours.to_value_with_id(uc::MSG_REQUEST, xid, self.pairing_id.as_deref()),
        )
        .await
        .context("stage system_info: sending request")?;
        let resp = self.recv_response(xid, "system_info").await?;
        tracing::debug!(?resp, "system_info response");
        Ok(SystemInfo::from_value(&resp))
    }

    /// P3/P4 pasteboard fetch: request the clipboard, parse the response into
    /// typed items.
    ///
    /// TODO(long-payload): items larger than ~10 KB are NOT delivered inline in
    /// this response on a real device — Apple uses a separate TLS-wrapped bulk
    /// channel for those (see `docs/architecture.md` §4.3). That path is
    /// deliberately not implemented yet; this handles the inline case only.
    pub async fn fetch_pasteboard(&mut self) -> Result<Pasteboard> {
        let xid = self.next_xid();
        let req_id = uuid::Uuid::new_v4().to_string();
        send_opack(
            &mut self.stream,
            &mut self.channel,
            &uc::build_pasteboard_request(xid, uc::CMD_PBPASTE2, &req_id, None),
        )
        .await
        .context("stage payload_request: sending pasteboard-fetch request")?;
        let resp = self.recv_response(xid, "payload_request").await?;
        uc::parse_pasteboard_response(&resp)
            .context("stage archive_decode: parsing pasteboard response")
    }

    fn next_xid(&mut self) -> u64 {
        let x = self.xid;
        self.xid += 1;
        x
    }
}

/// Connect to a companion-link peer over TCP and run Pair-Verify, returning the
/// encrypted session ready for the content exchanges.
///
/// Without AWDL up this is expected to fail at `connect` against a real device;
/// it works against [`run_mock_server`] / [`loopback_demo`].
pub async fn connect(
    host: &str,
    port: u16,
    identity: PairingIdentity,
) -> Result<Session<TcpStream>> {
    let stream = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        Ok::<_, anyhow::Error>(
            TcpStream::connect(crate::discover::socket_target(host, port).await?).await?,
        )
    })
    .await
    .context("TCP connection timed out")?
    .with_context(|| format!("connecting to companion-link at {host}:{port}"))?;
    stream.set_nodelay(true).ok();
    tokio::time::timeout(
        std::time::Duration::from_secs(15),
        Session::handshake(stream, identity),
    )
    .await
    .context("Pair-Verify timed out")?
    .context("companion-link Pair-Verify handshake failed")
}

/// Connect to a companion-link peer over TCP and run Transient Pair-Setup M1..M4
/// (`Flags=0x10`, `fixedPIN="public"`), returning the encrypted [`Session`].
pub async fn connect_transient(host: &str, port: u16, pin: &str) -> Result<Session<TcpStream>> {
    let stream = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        Ok::<_, anyhow::Error>(
            TcpStream::connect(crate::discover::socket_target(host, port).await?).await?,
        )
    })
    .await
    .context("TCP connection timed out")?
    .with_context(|| format!("connecting to companion-link at {host}:{port}"))?;
    stream.set_nodelay(true).ok();
    tokio::time::timeout(
        std::time::Duration::from_secs(15),
        Session::handshake_transient(stream, pin),
    )
    .await
    .context("Transient Pair-Setup timed out")?
    .context("companion-link Transient Pair-Setup handshake failed")
}

// ---------------------------------------------------------------------------
// In-process loopback mock server (the peer side, for tests + `pull --loopback`)
// ---------------------------------------------------------------------------

/// Run the server side of one companion-link session over `stream`: Pair-Verify
/// M1->M4, then answer a system-info exchange with `sysinfo` and a pasteboard
/// fetch with the canned `pasteboard`. This is a TEST/DEV peer, not a model of
/// Apple's real service; it exists so the whole client flow is exercisable
/// without hardware.
pub async fn run_mock_server<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    identity: PairingIdentity,
    sysinfo: SystemInfo,
    pasteboard: Pasteboard,
) -> Result<()> {
    let mut server = PairVerifyServer::new(identity);
    let m1 = read_packet(&mut stream).await.context("mock: reading M1")?;
    let m2 = server.process_m1(&m1).context("mock: building M2")?;
    write_packet(&mut stream, &m2)
        .await
        .context("mock: sending M2")?;
    let m3 = read_packet(&mut stream).await.context("mock: reading M3")?;
    let (m4, mut channel) = server
        .process_m3(&m3)
        .context("mock: verifying M3 / building M4")?;
    write_packet(&mut stream, &m4)
        .await
        .context("mock: sending M4")?;

    // P1/P2: read the client's system-info request, reply with ours.
    let client_info = recv_opack(&mut stream, &mut channel)
        .await
        .context("mock: reading P1")?;
    let info_xid = client_info
        .get(uc::K_XID)
        .and_then(Value::as_u64)
        .unwrap_or(0);
    send_opack(
        &mut stream,
        &mut channel,
        &sysinfo.to_value(uc::MSG_RESPONSE, info_xid),
    )
    .await
    .context("mock: sending P2")?;

    // P3/P4: read the pasteboard-fetch request, reply with the canned board.
    let req = recv_opack(&mut stream, &mut channel)
        .await
        .context("mock: reading P3")?;
    let xid = req.get(uc::K_XID).and_then(Value::as_u64).unwrap_or(0);
    let resp = uc::build_pasteboard_response(xid, &pasteboard)?;
    send_opack(&mut stream, &mut channel, &resp)
        .await
        .context("mock: sending P4")?;
    Ok(())
}

/// Generate a matched (client, server) identity pair that trust each other's
/// signing keys — the crypto precondition Pair-Verify assumes from an existing
/// same-Apple-ID pairing. Used by the loopback demo and tests.
pub fn matched_loopback_identities() -> (PairingIdentity, PairingIdentity) {
    use ed25519_dalek::SigningKey;
    let client_sk = SigningKey::generate(&mut rand::rngs::OsRng);
    let server_sk = SigningKey::generate(&mut rand::rngs::OsRng);
    let client_vk = client_sk.verifying_key();
    let server_vk = server_sk.verifying_key();
    let client = PairingIdentity {
        pairing_id: None,
        signing: client_sk,
        device_irk: [0u8; 16],
        peers: vec![("mock-server".into(), server_vk)],
        peer_irks: vec![],
    };
    let server = PairingIdentity {
        pairing_id: None,
        signing: server_sk,
        device_irk: [0u8; 16],
        peers: vec![("mock-client".into(), client_vk)],
        peer_irks: vec![],
    };
    (client, server)
}

/// Drive the full client flow against an in-process mock server over a real
/// loopback TCP socket, with no external network and no hardware. Returns the
/// peer system-info and the fetched pasteboard. Backs `ac-dc pull --loopback`.
pub async fn loopback_demo() -> Result<(SystemInfo, Pasteboard)> {
    use tokio::net::TcpListener;

    let (client_id, server_id) = matched_loopback_identities();

    let server_info = SystemInfo {
        name: "Loopback Mock".into(),
        model: "ac-dc,mock".into(),
        os_version: "0.0".into(),
    };
    let board = Pasteboard {
        items: vec![PasteboardItem {
            uti: "public.utf8-plain-text".into(),
            data: b"hello from the loopback mock pasteboard".to_vec(),
        }],
    };

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("binding loopback listener")?;
    let addr = listener.local_addr().context("loopback listener addr")?;

    let srv_info = server_info.clone();
    let srv_board = board.clone();
    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.context("mock: accept")?;
        sock.set_nodelay(true).ok();
        run_mock_server(sock, server_id, srv_info, srv_board).await
    });

    let mut session = connect(&addr.ip().to_string(), addr.port(), client_id).await?;
    let peer_info = session
        .system_info_exchange(&SystemInfo {
            name: "ac-dc (Linux)".into(),
            model: "ac-dc,client".into(),
            os_version: env!("CARGO_PKG_VERSION").into(),
        })
        .await?;
    let pasteboard = session.fetch_pasteboard().await?;

    server.await.context("joining mock server task")??;
    Ok((peer_info, pasteboard))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;
    use tokio::net::{TcpListener, TcpStream};

    #[test]
    fn framing_roundtrips_plain_and_encrypted() {
        // Plain packet: advertised length == body length.
        let plain = ContinuityPacket::new(PacketType::PairVerifyPublicKey, vec![1, 2, 3, 4, 5]);
        let wire = plain.serialize().unwrap();
        assert_eq!(u16::from_be_bytes([wire[2], wire[3]]) as usize, 5);

        // Encrypted packet: advertised length == body + 16; our reader must
        // recover the raw body length. Drive it through the async reader.
        let enc = ContinuityPacket::new(PacketType::EncryptedData, vec![0xAB; 40]);
        let enc_wire = enc.serialize().unwrap();
        assert_eq!(u16::from_be_bytes([enc_wire[2], enc_wire[3]]) as usize, 40);

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mut buf = std::io::Cursor::new(enc_wire);
            let back = read_packet(&mut buf).await.unwrap();
            assert_eq!(back.ptype, PacketType::EncryptedData);
            assert_eq!(back.body, vec![0xAB; 40]);
        });
    }

    #[tokio::test]
    async fn write_then_read_packet_over_duplex() {
        let (mut a, mut b) = duplex(4096);
        let pkt = ContinuityPacket::new(PacketType::Finished, vec![9, 8, 7]);
        write_packet(&mut a, &pkt).await.unwrap();
        let got = read_packet(&mut b).await.unwrap();
        assert_eq!(got.ptype, PacketType::Finished);
        assert_eq!(got.body, vec![9, 8, 7]);
    }

    #[test]
    fn pasteboard_request_response_opack_roundtrip() {
        let req = uc::build_pasteboard_request(42, uc::CMD_PBPASTE2, "req-1", Some(&[1, 2, 3]));
        let decoded_req = opack::decode(&opack::encode(&req)).unwrap();
        assert_eq!(
            decoded_req.get(uc::K_TYPE).and_then(Value::as_u64),
            Some(uc::MSG_REQUEST)
        );
        assert_eq!(
            decoded_req.get(uc::K_ID).and_then(Value::as_str),
            Some(uc::SEL_PAYLOAD_REQUEST)
        );
        assert_eq!(decoded_req.get(uc::K_XID).and_then(Value::as_u64), Some(42));
        let content = decoded_req.get(uc::K_CONTENT).unwrap();
        assert_eq!(
            content.get("rClientCommand").and_then(Value::as_str),
            Some("pbpaste2")
        );

        let board = Pasteboard {
            items: vec![
                PasteboardItem {
                    uti: "public.utf8-plain-text".into(),
                    data: b"clip!".to_vec(),
                },
                PasteboardItem {
                    uti: "public.png".into(),
                    data: vec![0x89, 0x50, 0x4E, 0x47],
                },
            ],
        };
        let resp = uc::build_pasteboard_response(42, &board).unwrap();
        let decoded = opack::decode(&opack::encode(&resp)).unwrap();
        let parsed = uc::parse_pasteboard_response(&decoded).unwrap();
        assert_eq!(parsed, board);
        assert_eq!(parsed.plain_text().as_deref(), Some("clip!"));
    }

    #[test]
    fn parse_rejects_malformed_pasteboard() {
        // Missing `_c`.
        let bad = Value::dict([(uc::K_TYPE, Value::Int(uc::MSG_RESPONSE))]);
        assert!(uc::parse_pasteboard_response(&bad).is_err());
        // `_c` present but lacks `rActPayload`.
        let bad2 = Value::dict([(uc::K_CONTENT, Value::dict([]))]);
        assert!(uc::parse_pasteboard_response(&bad2).is_err());
    }

    #[tokio::test]
    async fn end_to_end_over_loopback_tcp() {
        let (client_id, server_id) = matched_loopback_identities();
        let sysinfo = SystemInfo {
            name: "Peer".into(),
            model: "iPhone".into(),
            os_version: "26.0".into(),
        };
        let board = Pasteboard {
            items: vec![PasteboardItem {
                uti: "public.utf8-plain-text".into(),
                data: b"copied text".to_vec(),
            }],
        };

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (si, bd) = (sysinfo.clone(), board.clone());
        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            run_mock_server(sock, server_id, si, bd).await
        });

        let stream = TcpStream::connect(addr).await.unwrap();
        let mut session = Session::handshake(stream, client_id).await.unwrap();
        assert_eq!(session.peer_label(), Some("mock-server"));
        let peer = session
            .system_info_exchange(&SystemInfo {
                name: "Linux".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(peer, sysinfo);
        let got = session.fetch_pasteboard().await.unwrap();
        assert_eq!(got, board);
        assert_eq!(got.plain_text().as_deref(), Some("copied text"));
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn dispatcher_skips_events_and_surfaces_errors_or_xid_mismatch() {
        let (client_id, server_id) = matched_loopback_identities();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let srv = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut server = PairVerifyServer::new(server_id);
            let m1 = read_packet(&mut sock).await.unwrap();
            let m2 = server.process_m1(&m1).unwrap();
            write_packet(&mut sock, &m2).await.unwrap();
            let m3 = read_packet(&mut sock).await.unwrap();
            let (m4, mut ch) = server.process_m3(&m3).unwrap();
            write_packet(&mut sock, &m4).await.unwrap();

            // P1: send an unrelated event (_t=1) before the real response (_t=3).
            let p1 = recv_opack(&mut sock, &mut ch).await.unwrap();
            let x1 = p1.get(uc::K_XID).and_then(Value::as_u64).unwrap();
            let evt = Value::dict([(uc::K_TYPE, Value::Int(uc::MSG_EVENT))]);
            send_opack(&mut sock, &mut ch, &evt).await.unwrap();
            let resp = SystemInfo::default().to_value(uc::MSG_RESPONSE, x1);
            send_opack(&mut sock, &mut ch, &resp).await.unwrap();

            // P3: return a remote error.
            let p3 = recv_opack(&mut sock, &mut ch).await.unwrap();
            let x3 = p3.get(uc::K_XID).and_then(Value::as_u64).unwrap();
            let err_resp = Value::dict([
                (uc::K_TYPE, Value::Int(uc::MSG_RESPONSE)),
                (uc::K_XID, Value::Int(x3)),
                (uc::K_ERR_CODE, Value::Int(42)),
                (uc::K_ERR_MSG, Value::Str("denied".into())),
            ]);
            send_opack(&mut sock, &mut ch, &err_resp).await.unwrap();

            // Next request: return mismatched _x.
            let p4 = recv_opack(&mut sock, &mut ch).await.unwrap();
            let x4 = p4.get(uc::K_XID).and_then(Value::as_u64).unwrap();
            let bad_xid = Value::dict([
                (uc::K_TYPE, Value::Int(uc::MSG_RESPONSE)),
                (uc::K_XID, Value::Int(x4 + 99)),
            ]);
            send_opack(&mut sock, &mut ch, &bad_xid).await.unwrap();
        });

        let stream = TcpStream::connect(addr).await.unwrap();
        let mut session = Session::handshake(stream, client_id).await.unwrap();
        let _ = session
            .system_info_exchange(&SystemInfo::default())
            .await
            .unwrap();
        let err = session.fetch_pasteboard().await.unwrap_err().to_string();
        assert!(err.contains("denied"), "expected remote error, got: {err}");
        let err2 = session.fetch_pasteboard().await.unwrap_err().to_string();
        assert!(err2.contains("transaction id mismatch"), "got: {err2}");
        srv.await.unwrap();
    }

    #[tokio::test]
    async fn loopback_demo_runs_end_to_end() {
        let (peer, board) = loopback_demo().await.unwrap();
        assert_eq!(peer.name, "Loopback Mock");
        assert!(board
            .plain_text()
            .unwrap()
            .contains("loopback mock pasteboard"));
    }

    #[tokio::test]
    async fn untrusted_peer_is_rejected() {
        // Unknown signing keys must never authorize clipboard access.
        use ed25519_dalek::SigningKey;
        let client = PairingIdentity {
            pairing_id: None,
            signing: SigningKey::generate(&mut rand::rngs::OsRng),
            device_irk: [0u8; 16],
            peers: vec![], // knows no peers
            peer_irks: vec![],
        };
        let server = PairingIdentity {
            pairing_id: None,
            signing: SigningKey::generate(&mut rand::rngs::OsRng),
            device_irk: [0u8; 16],
            peers: vec![],
            peer_irks: vec![],
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            run_mock_server(sock, server, SystemInfo::default(), Pasteboard::default()).await
        });
        let stream = TcpStream::connect(addr).await.unwrap();
        let result = Session::handshake(stream, client).await;
        assert!(result.err().unwrap().to_string().contains("M2"));
        assert!(srv.await.unwrap().is_err());
    }
}
