//! Adhoc Cross-Device Connection protocol and diagnostic tools.
mod advert;
mod advertise;
mod airdrop;
mod clipboard_out;
mod companion;
mod companion_client;
mod contacts;
mod daemon;
mod discover;
mod gcm;
mod health;
mod ipc;
mod keystore;
mod opack;
mod orchestrate;
mod pair_setup;
mod pasteboard_archive;
mod radio_daemon;
mod scan;
mod tlv8;
mod transfer;
mod transfer_ble;
mod ui;
mod ui_widgets;

use anyhow::Result;
use clap::{ArgAction, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
enum ContactsAction {
    /// Add a trusted Apple ID email or phone number.
    Add { identifier: String },
    /// Remove a trusted Apple ID email or phone number.
    Remove { identifier: String },
    /// List configured trusted senders.
    List,
}

#[derive(Parser)]
#[command(name = "ac-dc", version, about)]
struct Cli {
    /// Increase diagnostic logging (-v enables debug, -vv enables trace). RUST_LOG overrides this.
    #[arg(short = 'v', long = "verbose", global = true, action = ArgAction::Count)]
    verbose: u8,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Run the user-scoped Airdrop-compatible protocol service.
    Daemon {
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Run the root-only AWDL radio broker (`/run/ac-dc/radio.sock`).
    RadioDaemon {
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
        #[arg(long, allow_hyphen_values = true)]
        name: Option<String>,
        #[arg(long, allow_hyphen_values = true)]
        directory: Option<PathBuf>,
        #[arg(long, allow_hyphen_values = true)]
        file: Vec<PathBuf>,
        #[arg(long, allow_hyphen_values = true)]
        link: Vec<String>,
        #[arg(long)]
        seconds: Option<u64>,
        #[arg(long)]
        ble_wake: Option<bool>,
        #[arg(long)]
        mode: Option<String>,
        #[arg(long)]
        once: bool,
        #[arg(long)]
        contacts_only: bool,
        #[arg(long, default_value_t = 30)]
        max_disk_percent: u8,
        #[arg(long)]
        copy_clipboard: Option<bool>,
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
    /// Collect a diagnostic report or toggle persistent daemon debug logging.
    Debug {
        #[arg(long)]
        json: bool,
        #[arg(long, conflicts_with = "disable")]
        enable: bool,
        #[arg(long, conflicts_with = "enable")]
        disable: bool,
    },
    /// Manage trusted Apple IDs / phone numbers for Contacts Only mode.
    Contacts {
        #[command(subcommand)]
        action: ContactsAction,
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
        /// Require Apple Contacts Only SenderRecordData.
        #[arg(long)]
        contacts_only: bool,
        /// Maximum transfer size as percentage of free disk space (1..=90).
        #[arg(long, default_value_t = 30)]
        max_disk_percent: u8,
        /// Copy received text, images, links, or saved paths to Wayland clipboard.
        #[arg(long)]
        copy_clipboard: Option<bool>,
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
        /// Use Transient Pair-Setup M1..M4 (`Flags=0x10`, `fixedPIN="public"`),
        /// which requires no exported `RPIdentity` keypair.
        #[arg(long)]
        transient: bool,
        /// Fresh JSONL from awdlctl events, used if mDNS resolution fails.
        #[arg(long, requires = "peer_mac")]
        events: Option<PathBuf>,
        #[arg(long)]
        peer_mac: Option<String>,
    },
    /// Pair with a companion-link peer via PairSetup (`_auTy=8 SessionPaired`, `pin="public"`)
    /// and save `rpidentity.json`.
    Pair {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 0)]
        port: u16,
        #[arg(long, default_value = "rpidentity.json")]
        out: PathBuf,
        #[arg(long)]
        pin_file: Option<PathBuf>,
        /// Client pairing UUID / IDS device UUID (e.g. an existing account IDS device UUID
        /// so `sharingd` recognizes the sender in `IDSService.devices`).
        #[arg(long)]
        pairing_id: Option<String>,
    },
    /// Watch real BLE copy events and pull from the matching companion-link peer.
    Auto {
        #[arg(long, default_value = "keys.json")]
        keys: PathBuf,
        /// Explicit mDNS instance; otherwise match the triggering BLE address to rpBA.
        #[arg(long)]
        instance: Option<String>,
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        notify: bool,
        /// RPIdentity keys for Pair-Verify (saved by `ac-dc pair`).
        #[arg(long, default_value = "rpidentity.json")]
        identity: PathBuf,
        /// Drive the wiring against the in-process loopback mock (no AWDL).
        #[arg(long)]
        loopback: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let default_filter = match cli.verbose {
        0 => "ac_dc=info",
        1 => "ac_dc=debug",
        _ => "ac_dc=trace",
    };
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| default_filter.into()),
        )
        .init();

    tracing::debug!(verbose = cli.verbose, command = ?cli.cmd, "ac-dc command starting");
    // `ui::run` owns the main thread for the window loop and drives its own
    // runtime, joining the engine with `block_on` once the window closes. That
    // panics inside a runtime context, so the UI must be dispatched before one
    // exists rather than from an async main.
    match cli.cmd {
        Cmd::Ui => ui::run(),
        cmd => tokio::runtime::Runtime::new()?.block_on(command(cmd)),
    }
}

async fn command(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Ui => unreachable!("dispatched by main before the runtime starts"),
        Cmd::Daemon { socket } => {
            daemon::run(socket.unwrap_or_else(daemon::default_socket)).await?
        }
        Cmd::RadioDaemon { socket } => {
            radio_daemon::run(socket.unwrap_or_else(|| PathBuf::from(radio_daemon::DEFAULT_SOCKET)))
                .await?
        }
        Cmd::Ctl {
            op,
            host,
            port,
            name,
            directory,
            file,
            link,
            seconds,
            ble_wake,
            mode,
            once,
            contacts_only,
            max_disk_percent,
            copy_clipboard,
        } => {
            if op == "subscribe" {
                daemon::ctl_subscribe(daemon::default_socket()).await?;
            } else {
                let parsed_mode = match mode.as_deref() {
                    Some("timed") => Some(ipc::ReceiveMode::Timed),
                    Some("once") => Some(ipc::ReceiveMode::Once),
                    Some("always") => Some(ipc::ReceiveMode::Always),
                    Some(other) => anyhow::bail!("unknown receive mode: {other}"),
                    None => None,
                };
                let reply = daemon::ctl(
                    daemon::default_socket(),
                    daemon::Request {
                        op,
                        host,
                        port,
                        name,
                        directory,
                        files: file,
                        links: link,
                        seconds,
                        ble_wake,
                        mode: parsed_mode,
                        once: once.then_some(true),
                        contacts_only: contacts_only.then_some(true),
                        max_disk_percent: Some(max_disk_percent),
                        copy_to_clipboard: copy_clipboard,
                        clipboard: None,
                    },
                )
                .await?;
                println!("{}", serde_json::to_string(&reply)?);
            }
        }
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
            contacts_only,
            max_disk_percent,
            copy_clipboard,
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
                open_destination: true,
                visibility: if contacts_only {
                    airdrop::VisibilityMode::ContactsOnly
                } else {
                    airdrop::VisibilityMode::Everyone
                },
                max_disk_percent,
                copy_to_clipboard: copy_clipboard.unwrap_or(true),
            })
            .await?;
        }
        Cmd::Contacts { action } => {
            let path = contacts::default_path();
            let mut store = contacts::TrustedSenders::load(&path)?;
            match action {
                ContactsAction::Add { identifier } => {
                    if store.add(&identifier)? {
                        store.save(&path)?;
                        println!("Added trusted sender.");
                    } else {
                        println!("Sender already in trusted list.");
                    }
                }
                ContactsAction::Remove { identifier } => {
                    if store.remove(&identifier)? {
                        store.save(&path)?;
                        println!("Removed trusted sender.");
                    } else {
                        println!("Sender not found in trusted list.");
                    }
                }
                ContactsAction::List => {
                    for entry in store.list() {
                        println!("{entry}");
                    }
                }
            }
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
        Cmd::Debug {
            json,
            enable,
            disable,
        } => {
            if enable || disable {
                health::set_daemon_debug(enable)?;
                println!(
                    "{}",
                    if enable {
                        "Debug logging enabled for ac-dc-daemon.service."
                    } else {
                        "Debug logging disabled for ac-dc-daemon.service."
                    }
                );
            } else {
                let report = health::debug_report(&daemon::default_socket()).await;
                print!("{}", health::format_debug_report(&report, json)?);
            }
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
            transient,
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

            let mut session = if transient {
                tracing::info!(%host, port, "connecting companion-link with Transient Pair-Setup (fixedPIN=\"public\")");
                companion_client::connect_transient(&host, port, "public").await?
            } else {
                let identity_path = identity.unwrap_or(keys);
                match companion::PairingIdentity::load(&identity_path) {
                    Ok(ident) => {
                        tracing::info!(%host, port, "connecting companion-link with Pair-Verify");
                        companion_client::connect(&host, port, ident).await?
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "no RPIdentity found; falling back to Transient Pair-Setup (fixedPIN=\"public\")");
                        companion_client::connect_transient(&host, port, "public").await?
                    }
                }
            };
            tracing::info!(peer = ?session.peer_label(), "Pair-Verify authenticated");
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
        Cmd::Pair {
            host,
            port,
            out,
            pin_file,
            pairing_id,
        } => {
            let (host, port) = discover::companion_link_target(&host, port).await?;
            let addr = discover::socket_target(&host, port).await?;
            let stream = tokio::net::TcpStream::connect(addr).await?;
            stream.set_nodelay(true).ok();
            let paired =
                pair_setup::run_pair_setup(stream, pairing_id.as_deref(), || async move {
                    if let Some(pf) = pin_file {
                        tracing::info!(path = %pf.display(), "PairSetup M2 received; waiting for PIN file");
                        let deadline =
                            tokio::time::Instant::now() + std::time::Duration::from_secs(120);
                        loop {
                            if let Ok(text) = tokio::fs::read_to_string(&pf).await {
                                let t = text.trim().to_string();
                                if !t.is_empty() {
                                    return Ok(t);
                                }
                            }
                            if tokio::time::Instant::now() >= deadline {
                                anyhow::bail!("timed out waiting for PIN file {}", pf.display());
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        }
                    } else {
                        Ok("public".to_string())
                    }
                })
                .await?;
            pair_setup::write_identity_json(&out, &paired)?;
            println!(
                "Paired with '{}' (pairing_id={}) and saved {}",
                paired.label,
                paired.pairing_id,
                out.display()
            );
        }
        Cmd::Auto {
            keys,
            identity,
            instance,
            host,
            port,
            loopback,
            notify,
        } => {
            orchestrate::run(orchestrate::AutoConfig {
                keys,
                identity_path: identity,
                instance,
                host,
                port,
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
