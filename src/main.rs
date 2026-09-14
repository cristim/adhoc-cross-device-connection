//! ac-dc: receive Apple Universal Clipboard / Handoff BLE announcements
//! on Linux, using encryption keys exported from a macOS install signed into
//! the same Apple ID.
//!
//! This first milestone covers DISCOVERY + DECRYPTION of the BLE advertisement:
//! it tells you, on Linux, the moment your iPhone or Mac copies something. The
//! subsequent milestones (pulling the actual clipboard content over the
//! companion-link service, then the reverse direction) are tracked in the
//! README roadmap.

mod advert;
mod advertise;
mod clipboard_out;
mod companion;
mod companion_client;
mod discover;
mod gcm;
mod keystore;
mod opack;
mod orchestrate;
mod scan;
mod tlv8;
mod transfer;
mod transfer_ble;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "ac-dc", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scan for and decrypt Handoff / Universal Clipboard BLE adverts.
    Scan {
        /// Path to the key file exported from macOS (see macos/export-keys.sh).
        #[arg(short, long, default_value = "keys.json")]
        keys: PathBuf,
    },
    /// Capture raw Handoff adverts as hex (no keys needed), to grab a packet
    /// for validating the decrypt against your own devices.
    Capture,
    /// Browse for `_companion-link._tcp` over mDNS (no keys needed). The live
    /// test of whether the M2 content channel is reachable over plain LAN vs.
    /// AWDL-only. See src/discover.rs.
    Discover,
    /// Receive an exported `keys.json` from a Mac over Bluetooth LE.
    ///
    /// Runs a GATT server; on the Mac run `ac-dc send-key`. An ephemeral X25519
    /// handshake protects the untrusted BLE link; both ends print a 6-digit
    /// code (SAS) that you must compare before confirming. keys.json is written
    /// (mode 0600) only after you confirm the codes matched.
    ReceiveKey {
        /// Where to write the received key file.
        #[arg(short, long, default_value = "keys.json")]
        out: PathBuf,
    },
    /// Decrypt a single advertisement supplied as a hex string (offline test).
    Decrypt {
        #[arg(short, long)]
        keys: PathBuf,
        /// Manufacturer data hex, e.g. "0c0e082a0099dead...".
        #[arg(short, long)]
        data: String,
    },
    /// Milestone 2: pull Universal Clipboard content over companion-link.
    ///
    /// Runs the transport-independent client: TCP connect, Pair-Verify M1-M4,
    /// then the system-info and pasteboard-fetch exchanges. Until AWDL is up
    /// (see docs/m2-awdl-plan.md) this will fail at `connect` against a real
    /// device — that is expected. Use `--loopback` to exercise the full flow
    /// against an in-process mock peer with no network.
    Pull {
        /// Peer address. Normally supplied later by `discover` over awdl0.
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Peer companion-link TCP port.
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// BLE keystore path (kept for CLI parity; used as the identity source
        /// if --identity is not given). See macos/export-keys.sh.
        #[arg(short, long, default_value = "keys.json")]
        keys: PathBuf,
        /// RPIdentity keys (Ed25519 signing key + known peers) for Pair-Verify.
        /// The exporter for these does not exist yet (docs/m2-awdl-plan.md §4.1).
        #[arg(long)]
        identity: Option<PathBuf>,
        /// Run against an in-process loopback mock peer (no network, no keys).
        #[arg(long)]
        loopback: bool,
    },
    /// Milestone 2 end-to-end orchestration: pull a copy onto the Linux
    /// clipboard, tying the M1 + M2 pieces together.
    ///
    /// Walks the state machine: scan detects "clipboard available" -> [AWDL
    /// bring-up] -> discover companion-link over awdl0 -> pull -> clipboard_out.
    /// The AWDL bring-up is an unimplemented gate today (awdl0 does not exist
    /// until the driver work lands), so the real flow stops there with a clear
    /// error. Use `--loopback` to drive the pull -> clipboard glue against an
    /// in-process mock peer — it actually lands text on your Wayland clipboard.
    Auto {
        /// RPIdentity keys for Pair-Verify (real flow only).
        #[arg(long, default_value = "keys.json")]
        identity: PathBuf,
        /// Drive the wiring against the in-process loopback mock (no AWDL).
        #[arg(long)]
        loopback: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ac_dc=info".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Scan { keys } => {
            let store = keystore::KeyStore::load(&keys)?;
            if store.is_empty() {
                tracing::warn!("no keys loaded; adverts will be detected but not decrypted");
            } else {
                tracing::info!(count = store.keys.len(), "loaded keys");
            }
            let scanner = scan::Scanner::new(store).await?;
            tokio::select! {
                r = scanner.run() => r?,
                _ = tokio::signal::ctrl_c() => tracing::info!("stopping"),
            }
        }
        Cmd::Capture => {
            let scanner = scan::Scanner::new_capture().await?;
            tokio::select! {
                r = scanner.run_capture() => r?,
                _ = tokio::signal::ctrl_c() => tracing::info!("stopping"),
            }
        }
        Cmd::Discover => {
            tokio::select! {
                r = discover::run() => r?,
                _ = tokio::signal::ctrl_c() => tracing::info!("stopping"),
            }
        }
        Cmd::ReceiveKey { out } => {
            tokio::select! {
                r = transfer_ble::receive_key(&out) => r?,
                _ = tokio::signal::ctrl_c() => tracing::info!("stopping"),
            }
        }
        Cmd::Decrypt { keys, data } => {
            let store = keystore::KeyStore::load(&keys)?;
            let bytes = hex::decode(data.trim())?;
            let ble = advert::HandoffBle::parse(&bytes)
                .ok_or_else(|| anyhow::anyhow!("not a Handoff advertisement"))?;
            let mut ok = false;
            for k in &store.keys {
                if let Some(plain) = gcm::open_truncated(
                    &k.key,
                    &ble.counter_iv,
                    &[ble.status],
                    &ble.ciphertext,
                    &ble.tag,
                )? {
                    let payload = advert::HandoffPayload::parse(&plain);
                    println!("decrypted with key {}: {}", k.id, hex::encode(&plain));
                    if let Some(p) = payload {
                        println!(
                            "  clipboard_available={} url={} activity_hash={}",
                            p.flags.clipboard_available(),
                            p.flags.has_url(),
                            hex::encode(p.activity_hash)
                        );
                    }
                    ok = true;
                    break;
                }
            }
            if !ok {
                println!("no key authenticated this advertisement");
            }
        }
        Cmd::Pull { host, port, keys, identity, loopback } => {
            if loopback {
                tracing::info!("running companion-link pull against in-process loopback mock");
                let (peer, board) = companion_client::loopback_demo().await?;
                print_pasteboard(&peer, &board);
                land_on_clipboard(&board);
                return Ok(());
            }

            // Once awdl0 is up, `companion_link_target` resolves
            // `_companion-link._tcp` scoped to it (a non-zero --port is still a
            // manual override). See src/discover.rs and docs/m2-awdl-plan.md §4.
            let (host, port) = discover::companion_link_target(&host, port).await?;

            // Pair-Verify needs the RPIdentity keys; fall back to --keys for CLI
            // parity (it will not parse as an identity yet — the exporter is a
            // follow-up, docs/m2-awdl-plan.md §4.1 — so this fails clearly).
            let identity_path = identity.unwrap_or(keys);
            let ident = companion::PairingIdentity::load(&identity_path)?;

            tracing::info!(%host, port, "connecting companion-link (expected to fail without AWDL)");
            let mut session = companion_client::connect(&host, port, ident).await?;
            let peer = session
                .system_info_exchange(&companion_client::SystemInfo {
                    name: "ac-dc (Linux)".into(),
                    model: "ac-dc,client".into(),
                    os_version: env!("CARGO_PKG_VERSION").into(),
                })
                .await?;
            let board = session.fetch_pasteboard().await?;
            print_pasteboard(&peer, &board);
            land_on_clipboard(&board);
        }
        Cmd::Auto { identity, loopback } => {
            orchestrate::run(orchestrate::AutoConfig { identity_path: identity, loopback }).await?;
        }
    }
    Ok(())
}

/// Put a successfully fetched pasteboard on the Wayland clipboard via
/// `clipboard_out`, printing what was copied. Best-effort: a clipboard failure
/// is logged, not fatal, since the fetch itself already succeeded.
fn land_on_clipboard(board: &companion_client::Pasteboard) {
    match clipboard_out::copy_pasteboard(board) {
        Ok(Some(outcome)) => println!("clipboard: copied {}", outcome.describe()),
        Ok(None) => println!("clipboard: nothing to copy (empty pasteboard)"),
        Err(e) => tracing::error!(error = %e, "failed to put pasteboard on the clipboard"),
    }
}

/// Print a fetched pasteboard and the peer's system info to stdout.
fn print_pasteboard(peer: &companion_client::SystemInfo, board: &companion_client::Pasteboard) {
    println!("peer: name={:?} model={:?} os={:?}", peer.name, peer.model, peer.os_version);
    if board.items.is_empty() {
        println!("pasteboard: (empty)");
    }
    for (i, item) in board.items.iter().enumerate() {
        match std::str::from_utf8(&item.data) {
            Ok(s) if item.uti.contains("text") => {
                println!("item[{i}] {} ({} bytes): {s:?}", item.uti, item.data.len());
            }
            _ => {
                println!(
                    "item[{i}] {} ({} bytes): {}",
                    item.uti,
                    item.data.len(),
                    hex::encode(&item.data)
                );
            }
        }
    }
}
