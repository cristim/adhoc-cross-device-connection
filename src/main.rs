//! Adhoc Cross-Device Connection protocol and diagnostic tools.
mod advert;
mod advertise;
mod airdrop;
mod clipboard_out;
mod companion;
mod companion_client;
mod daemon;
mod discover;
mod gcm;
mod health;
mod keystore;
mod opack;
mod orchestrate;
mod scan;
mod tlv8;
mod transfer;
mod transfer_ble;
mod ui;

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
    /// Run the privileged local Airdrop-compatible protocol service.
    Daemon {
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Send a JSON command to the local Airdrop-compatible service.
    Ctl {
        op: String,
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        file: Vec<PathBuf>,
        #[arg(long)]
        link: Vec<String>,
    },
    /// Open the native Airdrop-compatible send/receive window.
    Ui,
    /// Discover nearby Everyone-mode Airdrop-compatible recipients.
    Peers {
        #[arg(long, default_value = "awdl0")]
        iface: String,
        #[arg(long)]
        tls_identity: PathBuf,
    },
    /// Send files or HTTP(S) links to a selected Airdrop-compatible endpoint.
    Send {
        #[arg(long)]
        host: String,
        #[arg(long, default_value_t = 8770)]
        port: u16,
        #[arg(long)]
        tls_identity: PathBuf,
        #[arg(long, default_value = "Linux")]
        name: String,
        #[arg(long)]
        url: Vec<String>,
        files: Vec<PathBuf>,
    },
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
    Discover {
        #[arg(long, default_value = "awdl0")]
        iface: String,
        #[arg(long)]
        lan: bool,
    },
    /// Report missing runtime prerequisites without changing the radio.
    Doctor {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        keys: Option<PathBuf>,
        #[arg(long)]
        identity: Option<PathBuf>,
    },
    /// Report observed AWDL state; link readiness is not proof of transport.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Register captured/reconstructed Apple TLVs through BlueZ for a bounded experiment.
    Advertise {
        #[arg(
            long,
            required_unless_present = "airdrop_wake",
            conflicts_with = "airdrop_wake"
        )]
        data: Option<String>,
        #[arg(long)]
        airdrop_wake: bool,
        #[arg(long, default_value_t = 30)]
        seconds: u64,
        #[arg(long, default_value_t = 100)]
        interval_ms: u64,
    },
    /// Receive Airdrop-compatible links/files for a bounded Everyone-mode window (experimental).
    Receive {
        #[arg(long, default_value = "awdl0")]
        iface: String,
        #[arg(long)]
        directory: PathBuf,
        #[arg(long)]
        tls_identity: PathBuf,
        #[arg(long, default_value = "ac-dc")]
        name: String,
        #[arg(long, default_value_t = 8771)]
        port: u16,
        #[arg(long, default_value_t = 600)]
        seconds: u64,
        #[arg(long)]
        once: bool,
        #[arg(long)]
        notify: bool,
        /// Advertise a Bluetooth wake hint during receiving.
        #[arg(long)]
        ble_wake: bool,
    },
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
    /// (see docs/architecture.md) this will fail at `connect` against a real
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
        /// See macos/RPIDENTITY.md for the exporter and expected schema.
        #[arg(long)]
        identity: Option<PathBuf>,
        /// Run against an in-process loopback mock peer (no network, no keys).
        #[arg(long)]
        loopback: bool,
        /// Fresh JSONL from awdlctl events, used if mDNS resolution fails.
        #[arg(long, requires = "peer_mac")]
        events: Option<PathBuf>,
        #[arg(long)]
        peer_mac: Option<String>,
    },
    /// Watch real BLE copy events and pull from the matching companion-link peer.
    Auto {
        #[arg(long, default_value = "keys.json")]
        keys: PathBuf,
        /// Explicit mDNS instance; otherwise match the triggering BLE address to rpBA.
        #[arg(long)]
        instance: Option<String>,
        #[arg(long)]
        notify: bool,
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
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ac_dc=info".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Daemon { socket } => {
            daemon::run(socket.unwrap_or_else(daemon::default_socket)).await?
        }
        Cmd::Ctl {
            op,
            host,
            port,
            name,
            file,
            link,
        } => {
            daemon::ctl(
                daemon::default_socket(),
                daemon::Request {
                    op,
                    host,
                    port,
                    name,
                    files: file,
                    links: link,
                },
            )
            .await?
        }
        Cmd::Ui => ui::run().await?,
        Cmd::Peers {
            iface,
            tls_identity,
        } => {
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &airdrop::peers::browse(&iface, &tls_identity, 8).await?
                )?
            );
        }
        Cmd::Send {
            host,
            port,
            tls_identity,
            name,
            url,
            files,
        } => {
            let address = discover::socket_target(&host, port).await?;
            let client = airdrop::send::Client::new(address, &tls_identity)?;
            let receiver = client.discover().await?;
            tracing::info!(receiver, "Sending Airdrop-compatible transfer");
            client.transfer(&name, files, url).await?;
            println!("Transfer accepted by {receiver}");
        }
        Cmd::Receive {
            iface,
            directory,
            tls_identity,
            name,
            port,
            seconds,
            once,
            notify,
            ble_wake,
        } => {
            airdrop::run(airdrop::Config {
                radio_managed: false,
                ble_wake,
                approval: None,
                iface,
                directory,
                identity: tls_identity,
                name,
                port,
                seconds,
                once,
                notify,
            })
            .await?;
        }
        Cmd::Doctor {
            json,
            keys,
            identity,
        } => {
            let report = health::report(keys.as_deref(), identity.as_deref()).await;
            println!(
                "{}",
                if json {
                    serde_json::to_string(&report)?
                } else {
                    serde_json::to_string_pretty(&report)?
                }
            );
        }
        Cmd::Status { json } => {
            let report = health::radio().await?;
            println!(
                "{}",
                if json {
                    serde_json::to_string(&report)?
                } else {
                    serde_json::to_string_pretty(&report)?
                }
            );
        }
        Cmd::Advertise {
            data,
            airdrop_wake,
            seconds,
            interval_ms,
        } => {
            let data = if airdrop_wake {
                advertise::airdrop_wake()
            } else {
                hex::decode(data.unwrap_or_default())?
            };
            advertise::broadcast(data, seconds, interval_ms).await?;
        }
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
                _ = shutdown() => tracing::info!("stopping"),
            }
        }
        Cmd::Capture => {
            let scanner = scan::Scanner::new_capture().await?;
            tokio::select! {
                r = scanner.run_capture() => r?,
                _ = shutdown() => tracing::info!("stopping"),
            }
        }
        Cmd::Discover { iface, lan } => {
            tokio::select! {
                r = discover::run(if lan { None } else { Some(&iface) }) => r?,
                _ = shutdown() => tracing::info!("stopping"),
            }
        }
        Cmd::ReceiveKey { out } => {
            tokio::select! {
                r = transfer_ble::receive_key(&out) => r?,
                _ = shutdown() => tracing::info!("stopping"),
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
        Cmd::Pull {
            host,
            port,
            keys,
            identity,
            loopback,
            events,
            peer_mac,
        } => {
            if loopback {
                tracing::info!("running companion-link pull against in-process loopback mock");
                let (peer, board) = companion_client::loopback_demo().await?;
                print_pasteboard(&peer, &board);
                land_on_clipboard(&board);
                return Ok(());
            }

            // Once awdl0 is up, `companion_link_target` resolves
            // `_companion-link._tcp` scoped to it (a non-zero --port is still a
            // manual override). See src/discover.rs and docs/architecture.md §4.
            let (host, port) = match discover::companion_link_target(&host, port).await {
                Ok(target) => target,
                Err(e) => {
                    if let Some(path) = events {
                        tracing::warn!(error=%e, "mDNS failed; checking captured management-frame service records");
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)?
                            .as_millis() as u64;
                        discover::from_events(
                            &path,
                            peer_mac.as_deref().unwrap_or(""),
                            "awdl0",
                            now,
                        )?
                    } else {
                        return Err(e);
                    }
                }
            };

            // Pair-Verify uses a separate RPIdentity export, not the BLE key schema.
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
        Cmd::Auto {
            keys,
            identity,
            instance,
            loopback,
            notify,
        } => {
            orchestrate::run(orchestrate::AutoConfig {
                keys,
                identity_path: identity,
                instance,
                loopback,
                notify,
            })
            .await?;
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
    println!(
        "peer: name={:?} model={:?} os={:?}",
        peer.name, peer.model, peer.os_version
    );
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

/// Handle service termination as well as interactive cancellation.
async fn shutdown() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("SIGTERM handler");
    tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
}
