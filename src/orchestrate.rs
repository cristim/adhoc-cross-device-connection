//! BLE-triggered clipboard pull over LAN or AWDL, reporting status to the event daemon.
use crate::{
    clipboard_out,
    companion::PairingIdentity,
    companion_client::{self, SystemInfo},
    daemon, discover,
    ipc::{ClipboardStatus, Request},
    keystore::KeyStore,
    scan::Scanner,
};
use anyhow::{Context, Result};
use std::{path::PathBuf, time::Duration};

pub struct AutoConfig {
    pub keys: PathBuf,
    pub identity_path: PathBuf,
    pub instance: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub loopback: bool,
    pub notify: bool,
}

async fn report_daemon(status: ClipboardStatus) {
    let req = Request {
        op: "clipboard_report".into(),
        clipboard: Some(status),
        ..Default::default()
    };
    let _ = daemon::ctl(daemon::default_socket(), req).await;
}

pub async fn run(cfg: AutoConfig) -> Result<()> {
    if cfg.loopback {
        let (_, board) = companion_client::loopback_demo().await?;
        clipboard_out::copy_pasteboard(&board)?;
        return Ok(());
    }
    // Fail at startup on bad credentials; do not retry malformed key exports forever.
    PairingIdentity::load(&cfg.identity_path)?;
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let scanner = Scanner::new(KeyStore::load(&cfg.keys)?)
        .await?
        .with_events(tx);
    report_daemon(ClipboardStatus {
        available: true,
        state: "watching".into(),
        last_item_kind: None,
        last_error: None,
    })
    .await;

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
                let (host, port) = if let (Some(h), Some(p)) = (cfg.host.as_deref(), cfg.port) {
                    (h.to_string(), p)
                } else {
                    discover::resolve_preferred(
                        cfg.instance.as_deref(),
                        Some(&event.address),
                        Duration::from_secs(10),
                    )
                    .await?
                };
                let mut session = companion_client::connect(
                    &host,
                    port,
                    PairingIdentity::load(&cfg.identity_path)?,
                )
                .await?;
                tracing::info!(peer=?session.peer_label(), "Pair-Verify authenticated");
                session
                    .system_info_exchange(&SystemInfo {
                        name: "ac-dc (Linux)".into(),
                        model: "ac-dc,client".into(),
                        os_version: env!("CARGO_PKG_VERSION").into(),
                    })
                    .await?;
                let board = session.fetch_pasteboard().await?;
                let outcome = clipboard_out::copy_pasteboard(&board)?;
                if let Some(out) = outcome {
                    tracing::info!(
                        bytes = out.bytes,
                        copied = out.spawned,
                        "clipboard transfer complete"
                    );
                    report_daemon(ClipboardStatus {
                        available: true,
                        state: "synced".into(),
                        last_item_kind: Some(format!("{} ({} B)", out.uti, out.bytes)),
                        last_error: None,
                    })
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
                report_daemon(ClipboardStatus {
                    available: true,
                    state: "error".into(),
                    last_item_kind: None,
                    last_error: Some(msg),
                })
                .await;
            }
        }
        anyhow::Ok(())
    };
    tokio::select! {r=scanner.run()=>r, r=process=>r, _=crate::shutdown()=>Ok(())}
}
