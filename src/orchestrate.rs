//! BLE-triggered clipboard pull over LAN or AWDL, reporting status to the event daemon.
use crate::{
    clipboard_out,
    companion::PairingIdentity,
    companion_client::{self, SystemInfo},
    companion_server, daemon, discover,
    ipc::{ClipboardStatus, Request},
    keystore::KeyStore,
    scan::Scanner,
};
use anyhow::{Context, Result};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::Mutex;

pub struct AutoConfig {
    pub keys: PathBuf,
    pub identity_path: PathBuf,
    pub instance: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub loopback: bool,
    pub notify: bool,
}

/// True when a just-fetched pull's content digest matches the digest of
/// whatever we last actually landed on the clipboard from the Mac. An empty
/// or not-yet-known digest on either side never counts as a duplicate, so
/// the very first pull (and an empty pasteboard) always proceeds.
fn is_duplicate_pull(incoming: Option<[u8; 32]>, last_landed: Option<[u8; 32]>) -> bool {
    incoming.is_some() && incoming == last_landed
}

/// LAN first, as `resolve_preferred` prefers, but if the LAN-resolved
/// endpoint actively refuses the connection (not merely slow), retry once on
/// `awdl0` for the exact same instance rather than a different device: the
/// same Mac can still be reachable over AWDL even when its LAN port is
/// unusable. Both attempts run within the caller's own pull deadline.
async fn connect_via_discovery(
    instance: Option<&str>,
    identity_path: &std::path::Path,
) -> Result<companion_client::Session<tokio::net::TcpStream>> {
    let identity = PairingIdentity::load(identity_path)?;
    match discover::resolve_on(None, instance, Duration::from_secs(4)).await {
        Ok((host, port, fullname)) => {
            match companion_client::connect(&host, port, identity.clone()).await {
                Ok(session) => Ok(session),
                Err(lan_connect_err) => {
                    if !std::path::Path::new("/sys/class/net/awdl0").exists() {
                        return Err(lan_connect_err);
                    }
                    let (host, port, _) = discover::resolve_on(
                        Some("awdl0"),
                        Some(&fullname),
                        Duration::from_secs(5),
                    )
                    .await
                    .context("LAN connection failed and no AWDL fallback was found")?;
                    companion_client::connect(&host, port, identity).await
                }
            }
        }
        Err(lan_resolve_err) => {
            if !std::path::Path::new("/sys/class/net/awdl0").exists() {
                return Err(lan_resolve_err);
            }
            let (host, port, _) =
                discover::resolve_on(Some("awdl0"), instance, Duration::from_secs(10)).await?;
            companion_client::connect(&host, port, identity).await
        }
    }
}

async fn report_status(
    tx: Option<&tokio::sync::mpsc::UnboundedSender<ClipboardStatus>>,
    status: ClipboardStatus,
) {
    if let Some(ch) = tx {
        let _ = ch.send(status);
    } else {
        let req = Request {
            op: "clipboard_report".into(),
            clipboard: Some(status),
            ..Default::default()
        };
        let _ = daemon::ctl(daemon::default_socket(), req).await;
    }
}

/// Run an optional subsystem; if it ends or fails, log once and park so it cannot
/// win the `select!` and tear down Mac -> Linux reception.
async fn park_on_exit(
    name: &str,
    fut: impl std::future::Future<Output = Result<()>>,
) -> Result<()> {
    match fut.await {
        Ok(()) => tracing::warn!("{name} stopped; clipboard reception continues without it"),
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "{name} failed; clipboard reception continues without it")
        }
    }
    std::future::pending().await
}

pub async fn run(cfg: AutoConfig) -> Result<()> {
    run_with_reporter(cfg, None).await
}

pub async fn run_with_reporter(
    cfg: AutoConfig,
    status_tx: Option<tokio::sync::mpsc::UnboundedSender<ClipboardStatus>>,
) -> Result<()> {
    if cfg.loopback {
        let (_, board) = companion_client::loopback_demo().await?;
        clipboard_out::copy_pasteboard(&board)?;
        return Ok(());
    }
    // Fail at startup on bad credentials; do not retry malformed key exports forever.
    let server_identity = PairingIdentity::load(&cfg.identity_path)?;
    let store = KeyStore::load(&cfg.keys)?;
    let adv_keys = store.keys.clone();
    let echo_guard: companion_server::EchoGuard = Arc::new(Mutex::new(None));

    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let scanner = Scanner::new(store).await?.with_events(tx);
    report_status(
        status_tx.as_ref(),
        ClipboardStatus {
            available: true,
            state: "watching".into(),
            last_item_kind: None,
            last_error: None,
        },
    )
    .await;

    let pull_echo = echo_guard.clone();
    // Independent from `echo_guard`: the broadcaster clears that cell on any
    // local copy that doesn't match it (so a later genuine local copy isn't
    // suppressed forever), which would also erase this loop's memory of what
    // it last landed if the two shared one cell. That reopens the exact
    // overwrite race this dedupe exists to close: a local copy made between
    // two Mac-originated re-adverts of the same old content would clear the
    // shared guard, making the old content look "new" again on the next pull.
    let last_landed_from_mac: Arc<Mutex<Option<[u8; 32]>>> = Arc::new(Mutex::new(None));
    let process = async {
        while let Some(event) = rx.recv().await {
            tracing::info!(
                device=%event.device,
                key=%event.key_id,
                activity=%event.activity,
                flags=event.flags,
                iv=%hex::encode(event.iv),
                adv_len=event.raw_adv.len(),
                timestamp_ms=event.timestamp_ms,
                "copy detected"
            );
            let result = tokio::time::timeout(Duration::from_secs(40), async {
                let mut session = if let (Some(h), Some(p)) = (cfg.host.as_deref(), cfg.port) {
                    companion_client::connect(h, p, PairingIdentity::load(&cfg.identity_path)?)
                        .await?
                } else {
                    connect_via_discovery(cfg.instance.as_deref(), &cfg.identity_path).await?
                };
                tracing::info!(peer=?session.peer_label(), "Pair-Verify authenticated");
                session
                    .system_info_exchange(&SystemInfo {
                        name: "ac-dc (Linux)".into(),
                        model: "ac-dc,client".into(),
                        os_version: env!("CARGO_PKG_VERSION").into(),
                    })
                    .await?;
                let board = session.fetch_pasteboard().await?;
                let incoming_digest = clipboard_out::written_digest(&board);
                // The Mac re-broadcasts its "clipboard available" advert on a
                // timer that has nothing to do with a real copy: its rotating
                // nearby-action counter changes even when the pasteboard
                // content hasn't, so `scan.rs`'s (counter, activity) dedupe
                // alone can't tell a repeat from a new copy. Compare against
                // what we actually landed last time; a repeat must not
                // re-overwrite a genuinely new local copy made in between.
                if is_duplicate_pull(incoming_digest, *last_landed_from_mac.lock().await) {
                    tracing::debug!(
                        device = %event.device,
                        "duplicate clipboard content pulled from Mac; not re-copying"
                    );
                    return anyhow::Ok(());
                }
                let outcome = clipboard_out::copy_pasteboard(&board)?;
                // Only a write that actually reached the clipboard can echo
                // back to the Mac; a failed write (the `?` above) must not
                // leave a stale digest suppressing a later, genuine local copy.
                *pull_echo.lock().await = incoming_digest;
                *last_landed_from_mac.lock().await = incoming_digest;
                if let Some(out) = outcome {
                    tracing::info!(
                        bytes = out.bytes,
                        copied = out.spawned,
                        "clipboard transfer complete"
                    );
                    report_status(
                        status_tx.as_ref(),
                        ClipboardStatus {
                            available: true,
                            state: "synced".into(),
                            last_item_kind: Some(format!("{} ({} B)", out.uti, out.bytes)),
                            last_error: None,
                        },
                    )
                    .await;
                    if cfg.notify && out.spawned {
                        let _ = tokio::process::Command::new("notify-send")
                            .args(["Clipboard received", &format!("From {}", event.device)])
                            .kill_on_drop(true)
                            .status()
                            .await;
                    }
                }
                anyhow::Ok(())
            })
            .await
            .context("clipboard transaction timed out")
            .and_then(|r| r);
            if let Err(e) = result {
                let msg = format!("{e:#}");
                tracing::warn!(error=%msg, "clipboard pull failed; waiting for next copy");
                report_status(
                    status_tx.as_ref(),
                    ClipboardStatus {
                        available: true,
                        state: "error".into(),
                        last_item_kind: None,
                        last_error: Some(msg),
                    },
                )
                .await;
            }
        }
        anyhow::Ok(())
    };
    // A local-listener failure (bind, mDNS init, or an accept error) must not
    // end the whole clipboard task: an explicit remote endpoint lets pulls
    // continue without it, same as the broadcaster's own failure isolation.
    let server_fut = park_on_exit(
        "Companion-Link server",
        companion_server::run_server(server_identity, 0, None),
    );
    let broadcaster_fut = park_on_exit(
        "Linux -> Mac clipboard watcher",
        companion_server::run_clipboard_broadcaster(adv_keys, echo_guard),
    );
    tokio::select! {
        r = scanner.run() => r,
        r = process => r,
        r = server_fut => r,
        r = broadcaster_fut => r,
        _ = crate::shutdown() => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_failed_or_finished_subsystem_parks_instead_of_ending_the_select() {
        let wait = Duration::from_millis(150);
        let failing = park_on_exit("t", async { anyhow::bail!("wl-paste missing") });
        assert!(tokio::time::timeout(wait, failing).await.is_err());
        let finished = park_on_exit("t", async { Ok(()) });
        assert!(tokio::time::timeout(wait, finished).await.is_err());
        let no_keys = park_on_exit(
            "t",
            companion_server::run_clipboard_broadcaster(vec![], Arc::new(Mutex::new(None))),
        );
        assert!(tokio::time::timeout(wait, no_keys).await.is_err());
    }

    #[test]
    fn repeat_of_the_last_landed_digest_is_a_duplicate() {
        let d = [7u8; 32];
        assert!(is_duplicate_pull(Some(d), Some(d)));
    }

    #[test]
    fn a_different_digest_is_not_a_duplicate() {
        let a = [1u8; 32];
        let b = [2u8; 32];
        assert!(!is_duplicate_pull(Some(a), Some(b)));
    }

    #[test]
    fn the_first_ever_pull_is_never_a_duplicate() {
        assert!(!is_duplicate_pull(Some([1u8; 32]), None));
    }

    #[test]
    fn an_empty_pasteboard_is_never_a_duplicate() {
        // `written_digest` returns `None` for an empty pasteboard; it must
        // still reach `copy_pasteboard` so the existing "nothing to copy"
        // warning fires, not be silently swallowed as a repeat.
        assert!(!is_duplicate_pull(None, None));
        assert!(!is_duplicate_pull(None, Some([9u8; 32])));
    }
}
