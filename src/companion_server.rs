//! Companion-Link server (`_companion-link._tcp.local.`) and Wayland clipboard
//! broadcaster (`0x0c` BLE Handoff TLV) for Linux -> Mac Universal Clipboard.

use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, BufReader};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

use crate::advert::{HandoffBle, HandoffFlags, HandoffPayload};
use crate::clipboard_out;
use crate::companion::{PairVerifyServer, PairingIdentity};
use crate::companion_client::{read_packet, recv_opack, send_opack, write_packet, Pasteboard};
use crate::keystore::HandoffKey;
use crate::opack::Value;
use crate::pasteboard_archive;

const SERVICE_TYPE: &str = "_companion-link._tcp.local.";

/// Tracks the SHA-256 of the most recently pulled Mac clipboard so Linux -> Mac
/// does not re-broadcast an item we just pulled from the Mac.
pub type EchoGuard = Arc<Mutex<Option<[u8; 32]>>>;

pub fn hash_pasteboard(pb: &Pasteboard) -> [u8; 32] {
    let mut h = Sha256::new();
    for it in &pb.items {
        h.update(it.uti.as_bytes());
        h.update([0u8]);
        h.update((it.data.len() as u64).to_le_bytes());
        h.update(&it.data);
    }
    h.finalize().into()
}

/// Serve one Companion-Link peer over `stream` (`PairVerify M1..M4`, then
/// encrypted `_systemInfo` and `com.apple.handoff.payload-request`).
pub async fn serve_connection<S, F>(
    mut stream: S,
    identity: PairingIdentity,
    mut get_pasteboard: F,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: FnMut() -> Result<Pasteboard>,
{
    let pairing_id = identity.pairing_id.clone();
    let mut server = PairVerifyServer::new(identity);
    let m1 = read_packet(&mut stream)
        .await
        .context("server: reading PairVerify M1")?;
    let m2 = server
        .process_m1(&m1)
        .context("server: building PairVerify M2")?;
    write_packet(&mut stream, &m2)
        .await
        .context("server: sending PairVerify M2")?;
    let m3 = read_packet(&mut stream)
        .await
        .context("server: reading PairVerify M3")?;
    let (m4, mut channel) = server
        .process_m3(&m3)
        .context("server: verifying PairVerify M3")?;
    write_packet(&mut stream, &m4)
        .await
        .context("server: sending PairVerify M4")?;

    loop {
        let req = match recv_opack(&mut stream, &mut channel).await {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!(error = %e, "companion-link peer closed session");
                break;
            }
        };
        let msg_type = req.get("_t").and_then(Value::as_u64);
        if msg_type != Some(2) {
            continue;
        }
        let xid = req.get("_x").and_then(Value::as_u64).unwrap_or(0);
        let req_id = req.get("_i").and_then(Value::as_str).unwrap_or("");

        let resp = match req_id {
            "_systemInfo" => build_system_info_response(xid, pairing_id.as_deref()),
            "com.apple.handoff.payload-request" => {
                let cmd = req
                    .get("_c")
                    .and_then(|c| c.get("rClientCommand"))
                    .and_then(Value::as_str)
                    .unwrap_or("pbpaste2");
                tracing::info!(cmd, "serving Universal Clipboard payload request to Mac");
                let pb = get_pasteboard().unwrap_or_default();
                let archive = pasteboard_archive::encode_wrapper(&pb)?;
                Value::dict([
                    ("_t", Value::Int(3)),
                    ("_x", Value::Int(xid)),
                    ("_rT", Value::Int(5)),
                    ("_c", Value::dict([("rActPayload", Value::Bytes(archive))])),
                ])
            }
            other => {
                tracing::debug!(request_id = other, "unhandled companion-link request");
                Value::dict([
                    ("_t", Value::Int(3)),
                    ("_x", Value::Int(xid)),
                    ("_ec", Value::Int(58822)), // -6714 kNotHandledErr
                    ("_em", Value::Str("No request handler".into())),
                ])
            }
        };
        send_opack(&mut stream, &mut channel, &resp).await?;
    }
    Ok(())
}

fn build_system_info_response(xid: u64, pairing_id: Option<&str>) -> Value {
    let mut content = vec![
        (
            Value::Str("name".into()),
            Value::Str("ac-dc (Linux)".into()),
        ),
        (Value::Str("model".into()), Value::Str("ac-dc,Linux".into())),
        (
            Value::Str("osVersion".into()),
            Value::Str(env!("CARGO_PKG_VERSION").into()),
        ),
        (Value::Str("_sv".into()), Value::Str("715.2".into())),
        (Value::Str("_bf".into()), Value::Int(0)),
    ];
    if let Some(id) = pairing_id {
        content.push((Value::Str("_i".into()), Value::Str(id.into())));
        content.push((Value::Str("_idsID".into()), Value::Str(id.into())));
        content.push((Value::Str("_spID".into()), Value::Str(id.into())));
    }
    Value::dict([
        ("_t", Value::Int(3)),
        ("_x", Value::Int(xid)),
        ("_c", Value::Dict(content)),
    ])
}

/// Publish `_companion-link._tcp.local.` over LAN mDNS and serve incoming
/// `PairVerify` + `com.apple.handoff.payload-request` connections.
pub async fn run_server(
    identity: PairingIdentity,
    port: u16,
    ble_addr: Option<String>,
) -> Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port))
        .await
        .with_context(|| format!("binding companion-link server on port {port}"))?;
    let local_port = listener.local_addr()?.port();

    let mdns = ServiceDaemon::new().context("starting mDNS daemon for companion-link")?;
    let mut props = HashMap::new();
    props.insert("rpMd".into(), "MacBookPro18,3".into());
    props.insert("rpVr".into(), "715.2".into());
    props.insert("rpFl".into(), "0x20000".into());
    props.insert("rpHN".into(), "ac-dc".into());
    if let Some(ba) = ble_addr {
        props.insert("rpBA".into(), ba);
    }
    let host_label = "ac-dc.local.";
    let svc_info = ServiceInfo::new(
        SERVICE_TYPE,
        "ac-dc",
        host_label,
        (),
        local_port,
        Some(props),
    )
    .context("building companion-link mDNS ServiceInfo")?
    .enable_addr_auto();
    let fullname = svc_info.get_fullname().to_string();
    mdns.register(svc_info)
        .context("registering _companion-link._tcp.local.")?;
    tracing::info!(port = local_port, instance = %fullname, "companion-link server listening");

    let res = loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (sock, peer_addr) = accepted?;
                sock.set_nodelay(true).ok();
                tracing::debug!(%peer_addr, "accepted companion-link connection");
                let id = identity.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve_connection(sock, id, clipboard_out::read_clipboard).await {
                        tracing::debug!(%peer_addr, error = %format!("{e:#}"), "companion-link client ended");
                    }
                });
            }
            _ = crate::shutdown() => break Ok(()),
        }
    };

    let _ = mdns.unregister(&fullname);
    let _ = mdns.shutdown();
    res
}

/// Watch Wayland clipboard changes via `wl-paste --watch` and broadcast
/// encrypted `0x0c` BLE Handoff advertisements when the local user copies.
pub async fn run_clipboard_broadcaster(keys: Vec<HandoffKey>, echo_guard: EchoGuard) -> Result<()> {
    if keys.is_empty() {
        anyhow::bail!("no Continuity keys available for BLE broadcasting");
    }
    if std::env::var_os("WAYLAND_DISPLAY").is_none_or(|v| v.is_empty()) {
        tracing::info!("WAYLAND_DISPLAY unset; skipping Linux -> Mac clipboard watcher");
        return std::future::pending().await;
    }

    let mut child = tokio::process::Command::new("wl-paste")
        .args(["--watch", "printf", "changed\\n"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("spawning `wl-paste --watch` (is wl-clipboard installed?)")?;
    let stdout = child.stdout.take().context("wl-paste stdout missing")?;
    let mut lines = BufReader::new(stdout).lines();

    let mut adv_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut counter: u16 = rand::random();
    let key = keys[0].clone();

    while let Some(_line) = lines.next_line().await? {
        let Ok(pb) = clipboard_out::read_clipboard() else {
            continue;
        };
        if pb.items.is_empty() {
            continue;
        }
        let digest = hash_pasteboard(&pb);
        if echo_guard.lock().await.as_ref() == Some(&digest) {
            tracing::debug!("skipping BLE broadcast for clipboard just pulled from Mac");
            continue;
        }

        counter = counter.wrapping_add(1);
        let mut activity_hash = [0u8; 7];
        activity_hash.copy_from_slice(&digest[..7]);
        let payload = HandoffPayload {
            status: 0x00,
            activity_hash,
            flags: HandoffFlags::from_byte(0x08),
        };
        let tlv = match HandoffBle::seal(&key.key, counter.to_le_bytes(), &payload) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "failed to seal Handoff BLE advert");
                continue;
            }
        };

        if let Some(prev) = adv_task.take() {
            prev.abort();
        }
        let key_id = key.id.clone();
        adv_task = Some(tokio::spawn(async move {
            tracing::info!(key = %key_id, counter, "broadcasting 0x0c Universal Clipboard BLE advert");
            if let Err(e) = crate::advertise::broadcast(tlv, 20, 100).await {
                tracing::warn!(error = %format!("{e:#}"), "BLE 0x0c broadcast failed");
            }
        }));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::companion_client::{
        matched_loopback_identities, PasteboardItem, Session, SystemInfo,
    };
    use tokio::net::TcpStream;

    #[tokio::test]
    async fn server_serves_system_info_and_pasteboard_over_loopback() {
        let (mut client_id, mut server_id) = matched_loopback_identities();
        client_id.pairing_id = Some("7F6D9CC4-31D5-4747-9B52-643A065CEB81".into());
        server_id.pairing_id = Some("E94331CD-F333-4532-BFC8-04606FA2C772".into());

        let expected = Pasteboard {
            items: vec![PasteboardItem {
                uti: "public.utf8-plain-text".into(),
                data: b"linux to mac clipboard!".to_vec(),
            }],
        };

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let pb_clone = expected.clone();
        let srv = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            serve_connection(sock, server_id, move || Ok(pb_clone.clone())).await
        });

        let stream = TcpStream::connect(addr).await.unwrap();
        let mut session = Session::handshake(stream, client_id).await.unwrap();
        let info = session
            .system_info_exchange(&SystemInfo {
                name: "Mac".into(),
                model: "MacBookPro18,3".into(),
                os_version: "26.1".into(),
            })
            .await
            .unwrap();
        assert_eq!(info.name, "ac-dc (Linux)");

        let got = session.fetch_pasteboard().await.unwrap();
        assert_eq!(got, expected);
        drop(session);
        srv.await.unwrap().unwrap();
    }
}
