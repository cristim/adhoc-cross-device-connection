//! Companion-Link server (`_companion-link._tcp.local.`) and Wayland clipboard
//! broadcaster (`0x0c` BLE Handoff TLV) for Linux -> Mac Universal Clipboard.

use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceInfo};
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
/// Largest archive we serve inline; the peer rejects frames over 4 MiB.
const MAX_ARCHIVE_PAYLOAD: usize = 3 * 1024 * 1024;
/// Caps concurrently-authenticating connections so a busy or untrusted LAN
/// cannot exhaust sockets/memory by opening many Pair-Verify sessions at once.
const MAX_ACTIVE_CONNECTIONS: usize = 16;

/// Admit one connection against `limiter`, or `None` if the server is already
/// serving `MAX_ACTIVE_CONNECTIONS` peers. The returned permit must be held
/// for the connection's whole lifetime and dropped when it ends.
fn admit_connection(
    limiter: &Arc<tokio::sync::Semaphore>,
) -> Option<tokio::sync::OwnedSemaphorePermit> {
    limiter.clone().try_acquire_owned().ok()
}

/// Tracks the digest of the representation last written from a Mac pull
/// (`clipboard_out::written_digest`) so Linux -> Mac does not re-broadcast it.
pub type EchoGuard = Arc<Mutex<Option<[u8; 32]>>>;

/// Serve one Companion-Link peer over `stream` (`PairVerify M1..M4`, then
/// encrypted `_systemInfo` and `com.apple.handoff.payload-request`).
pub async fn serve_connection<S, F, Fut>(
    mut stream: S,
    identity: PairingIdentity,
    mut get_pasteboard: F,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Pasteboard>>,
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
                match get_pasteboard()
                    .await
                    .and_then(|pb| pasteboard_archive::encode_wrapper(&pb))
                {
                    Ok(archive) if archive.len() > MAX_ARCHIVE_PAYLOAD => {
                        tracing::warn!(bytes = archive.len(), "clipboard too large to serve");
                        error_response(xid, "Clipboard too large")
                    }
                    Ok(archive) => Value::dict([
                        ("_t", Value::Int(3)),
                        ("_x", Value::Int(xid)),
                        ("_rT", Value::Int(5)),
                        ("_c", Value::dict([("rActPayload", Value::Bytes(archive))])),
                    ]),
                    Err(e) => {
                        tracing::warn!(error = %format!("{e:#}"), "reading clipboard for Mac failed");
                        error_response(xid, "Clipboard read failed")
                    }
                }
            }
            other => {
                tracing::debug!(request_id = other, "unhandled companion-link request");
                error_response(xid, "No request handler")
            }
        };
        send_opack(&mut stream, &mut channel, &resp).await?;
    }
    Ok(())
}

fn error_response(xid: u64, message: &str) -> Value {
    Value::dict([
        ("_t", Value::Int(3)),
        ("_x", Value::Int(xid)),
        ("_ec", Value::Int(58822)), // -6714 kNotHandledErr
        ("_em", Value::Str(message.into())),
    ])
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
    tracing::debug!(txt = %txt_summary(&props), "companion-link mDNS TXT record");
    let host_label = "ac-dc.local.";
    let svc_info = ServiceInfo::new(
        SERVICE_TYPE,
        crate::discover::OWN_INSTANCE,
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
    // Cleans up on every exit path (an accept error's `?`, pause dropping this
    // future, or the normal shutdown break below), not only the one below:
    // mdns-sd's worker keeps advertising until explicitly told to stop, so
    // just letting `mdns` go out of scope would leave a closed port advertised.
    let _mdns_registration = MdnsRegistration { mdns, fullname };

    let active_connections = Arc::new(tokio::sync::Semaphore::new(MAX_ACTIVE_CONNECTIONS));
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (sock, peer_addr) = accepted?;
                let Some(permit) = admit_connection(&active_connections) else {
                    tracing::debug!(%peer_addr, "rejecting companion-link connection: too many active connections");
                    continue;
                };
                sock.set_nodelay(true).ok();
                tracing::debug!(%peer_addr, "accepted companion-link connection");
                let id = identity.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(e) = serve_connection(sock, id, clipboard_out::read_clipboard).await {
                        tracing::debug!(%peer_addr, error = %format!("{e:#}"), "companion-link client ended");
                    }
                });
            }
            _ = crate::shutdown() => return Ok(()),
        }
    }
}

/// Unregisters and shuts down the mDNS worker on drop, so the service cannot
/// outlive `run_server` no matter which path it exits through.
struct MdnsRegistration {
    mdns: ServiceDaemon,
    fullname: String,
}

impl Drop for MdnsRegistration {
    fn drop(&mut self) {
        let _ = self.mdns.unregister(&self.fullname);
        let _ = self.mdns.shutdown();
    }
}

/// TXT keys a real Apple device publishes; `absent=` lists the ones we omit so
/// a Mac-side comparison shows what is missing. Identifying values are never logged.
const APPLE_TXT_KEYS: [&str; 6] = ["rpAD", "rpBA", "rpHA", "rpHI", "rpMac", "rpHN"];

/// One-line view of a companion-link TXT record: plain values for the fields
/// we set, only the length for device-identifying ones (`rpBA`, `rpAD`).
fn txt_summary(props: &HashMap<String, String>) -> String {
    let mut keys: Vec<_> = props.keys().collect();
    keys.sort();
    let mut parts: Vec<String> = keys
        .into_iter()
        .map(|k| match k.as_str() {
            "rpBA" | "rpAD" => format!("{k}=<len {}>", props[k].len()),
            _ => format!("{k}={}", props[k]),
        })
        .collect();
    let missing: Vec<&str> = APPLE_TXT_KEYS
        .into_iter()
        .filter(|k| !props.contains_key(*k))
        .collect();
    parts.push(format!("absent=[{}]", missing.join(",")));
    parts.join(" ")
}

/// Aborts the wrapped task when dropped, so a stale or leaked advert cannot outlive its watcher.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Checks `digest` against the digest recorded for the last successful Mac
/// pull, consuming that record once its suppression window has passed: a
/// non-matching digest proves the local clipboard moved on, so a later
/// coincidental repeat of the old digest (e.g. the user re-copying the same
/// item later) must broadcast normally instead of staying suppressed.
async fn is_echo_of_last_pull(echo_guard: &EchoGuard, digest: &[u8; 32]) -> bool {
    let mut guard = echo_guard.lock().await;
    if guard.as_ref() == Some(digest) {
        return true;
    }
    *guard = None;
    false
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

    // Held only so replacing or dropping it aborts the advert.
    let mut _adv_task: Option<AbortOnDrop> = None;
    let mut counter: u16 = rand::random();
    let key = keys[0].clone();

    while let Some(_line) = lines.next_line().await? {
        let pb = match clipboard_out::read_clipboard().await {
            Ok(pb) => pb,
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "not broadcasting: reading the clipboard failed");
                continue;
            }
        };
        let Some(digest) = clipboard_out::read_digest(&pb) else {
            continue;
        };
        if is_echo_of_last_pull(&echo_guard, &digest).await {
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

        _adv_task = None;
        let key_id = key.id.clone();
        tracing::debug!(
            key_index = 0,
            keys_total = keys.len(),
            status = format_args!("{:#04x}", tlv[2]),
            flags = format_args!("{:#04x}", payload.flags.raw),
            counter,
            tlv_len = tlv.len(),
            "0x0c advert content"
        );
        _adv_task = Some(AbortOnDrop(tokio::spawn(async move {
            tracing::info!(key = %key_id, counter, "broadcasting 0x0c Universal Clipboard BLE advert");
            if let Err(e) = crate::advertise::broadcast(tlv, 20, 100).await {
                tracing::warn!(error = %format!("{e:#}"), "BLE 0x0c broadcast failed");
            }
        })));
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

    #[test]
    fn txt_summary_hides_identifying_values_and_lists_what_is_absent() {
        let props: HashMap<String, String> = [
            ("rpMd", "MacBookPro18,3"),
            ("rpHN", "ac-dc"),
            ("rpBA", "AA:BB:CC:DD:EE:FF"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let line = txt_summary(&props);
        assert!(line.contains("rpMd=MacBookPro18,3"));
        assert!(line.contains("rpBA=<len 17>"));
        assert!(!line.contains("AA:BB"));
        assert!(line.ends_with("absent=[rpAD,rpHA,rpHI,rpMac]"));
    }

    #[test]
    fn admit_connection_rejects_once_the_limit_is_held_and_admits_again_after_release() {
        let limiter = Arc::new(tokio::sync::Semaphore::new(2));
        let a = admit_connection(&limiter).expect("first connection admitted");
        let b = admit_connection(&limiter).expect("second connection admitted");
        assert!(
            admit_connection(&limiter).is_none(),
            "a third concurrent connection must be rejected at the limit"
        );
        drop(a);
        assert!(
            admit_connection(&limiter).is_some(),
            "releasing a permit must free a slot for the next connection"
        );
        drop(b);
    }

    #[test]
    fn dropping_mdns_registration_stops_the_daemon_worker() {
        let mdns = ServiceDaemon::new().unwrap();
        let make_svc = |instance: &str, port: u16| {
            ServiceInfo::new(
                "_acdc-test._tcp.local.",
                instance,
                "acdc-test.local.",
                (),
                port,
                None,
            )
            .unwrap()
            .enable_addr_auto()
        };
        mdns.register(make_svc("first", 12345)).unwrap();
        let probe = mdns.clone();
        drop(MdnsRegistration {
            mdns,
            fullname: "first._acdc-test._tcp.local.".to_string(),
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        // The worker thread has exited on drop, so its command channel is
        // disconnected: a further command on the same handle must fail
        // instead of silently being accepted by a still-running worker.
        assert!(
            probe.register(make_svc("second", 12346)).is_err(),
            "the mDNS worker must be stopped once MdnsRegistration is dropped"
        );
    }

    #[tokio::test]
    async fn echo_suppression_does_not_outlive_an_intervening_local_copy() {
        let digest_a = [1u8; 32];
        let digest_b = [2u8; 32];
        let echo_guard: EchoGuard = Arc::new(Mutex::new(Some(digest_a)));

        // The Mac's own write of A echoes back to the watcher: suppressed.
        assert!(is_echo_of_last_pull(&echo_guard, &digest_a).await);
        // A real local copy of B does not match: not suppressed, and this
        // closes A's suppression window.
        assert!(!is_echo_of_last_pull(&echo_guard, &digest_b).await);
        // Re-copying A later is a fresh user action, not an echo of the pull
        // that happened before B: the stale digest must not suppress it.
        assert!(!is_echo_of_last_pull(&echo_guard, &digest_a).await);
    }

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
            serve_connection(sock, server_id, move || {
                let pb = pb_clone.clone();
                async move { Ok(pb) }
            })
            .await
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

    async fn fetch_from_server<F, Fut>(get: F) -> Result<Pasteboard>
    where
        F: FnMut() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<Pasteboard>> + Send,
    {
        let (mut client_id, mut server_id) = matched_loopback_identities();
        client_id.pairing_id = Some("7F6D9CC4-31D5-4747-9B52-643A065CEB81".into());
        server_id.pairing_id = Some("E94331CD-F333-4532-BFC8-04606FA2C772".into());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            serve_connection(sock, server_id, get).await
        });
        let stream = TcpStream::connect(addr).await.unwrap();
        let mut session = Session::handshake(stream, client_id).await.unwrap();
        let got = session.fetch_pasteboard().await;
        drop(session);
        srv.await.unwrap().unwrap();
        got
    }

    #[tokio::test]
    async fn clipboard_read_failure_is_a_request_error_not_an_empty_board() {
        let Err(err) =
            fetch_from_server(|| async { Err(anyhow::anyhow!("wl-paste stalled")) }).await
        else {
            panic!("a failed read must not look like an empty clipboard");
        };
        assert!(
            format!("{err:#}").contains("Clipboard read failed"),
            "got: {err:#}"
        );
    }

    #[test]
    fn largest_readable_text_board_still_fits_the_archive_cap() {
        // `read_clipboard` returns text under two UTIs, so the archive holds it twice.
        let text = vec![b'a'; clipboard_out::MAX_PASTE_BYTES];
        let board = Pasteboard {
            items: vec![
                PasteboardItem {
                    uti: "public.utf8-plain-text".into(),
                    data: text.clone(),
                },
                PasteboardItem {
                    uti: "public.text".into(),
                    data: text,
                },
            ],
        };
        let archive = pasteboard_archive::encode_wrapper(&board).unwrap();
        assert!(
            archive.len() <= MAX_ARCHIVE_PAYLOAD,
            "{} bytes",
            archive.len()
        );
    }

    #[tokio::test]
    async fn oversized_clipboard_is_a_request_error_not_a_broken_frame() {
        let big = Pasteboard {
            items: vec![PasteboardItem {
                uti: "public.utf8-plain-text".into(),
                data: vec![b'a'; MAX_ARCHIVE_PAYLOAD + 1024],
            }],
        };
        let Err(err) = fetch_from_server(move || {
            let big = big.clone();
            async move { Ok(big) }
        })
        .await
        else {
            panic!("an oversized board must be refused");
        };
        assert!(
            format!("{err:#}").contains("Clipboard too large"),
            "got: {err:#}"
        );
    }
}
