//! Adhoc Cross-Device Connection protocol and diagnostic tools.
mod advert;
mod advertise;
mod airdrop;
mod clipboard_out;
mod companion;
mod companion_client;
mod companion_server;
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
        /// Binds an `approve`/`reject` decision to the transfer id the
        /// caller is currently displaying.
        #[arg(long)]
        id: Option<u64>,
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
        /// Path to the key file exported from macOS (defaults to ~/.local/share/ac-dc/keys.json).
        #[arg(short, long)]
        keys: Option<PathBuf>,
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
        /// Where to write the received key file (defaults to ~/.local/share/ac-dc/keys.json).
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Also auto-pair over Companion-Link LAN mDNS if `ids_device_id` is included.
        #[arg(long, default_value_t = true, action = ArgAction::Set)]
        auto_pair: bool,
    },
    /// Decrypt a single advertisement supplied as a hex string (offline test).
    Decrypt {
        #[arg(short, long)]
        keys: Option<PathBuf>,
        /// Manufacturer data hex, e.g. "0c0e082a0099dead...".
        #[arg(short, long)]
        data: String,
    },
    /// Milestone 2: pull Universal Clipboard content over companion-link.
    Pull {
        /// Peer address; needs --port. Normally auto-discovered via mDNS.
        #[arg(long, default_value = "127.0.0.1", requires = "port")]
        host: String,
        /// Peer companion-link TCP port (0 = auto-discover via LAN/AWDL mDNS).
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// BLE keystore path (fallback if --identity is not given).
        #[arg(short, long)]
        keys: Option<PathBuf>,
        /// RPIdentity keys (saved by `ac-dc pair`, defaults to ~/.local/share/ac-dc/rpidentity.json).
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
        #[arg(long, default_value = "127.0.0.1", requires = "port")]
        host: String,
        #[arg(long, default_value_t = 0)]
        port: u16,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long)]
        pin_file: Option<PathBuf>,
        /// Client pairing UUID / IDS device UUID (defaults to `ids_device_id` from keys.json if present).
        #[arg(long)]
        pairing_id: Option<String>,
    },
    /// Watch real BLE copy events and pull from the matching companion-link peer.
    Auto {
        #[arg(long)]
        keys: Option<PathBuf>,
        /// Explicit mDNS instance; otherwise the first non-self companion-link service.
        #[arg(long)]
        instance: Option<String>,
        #[arg(long, requires = "port")]
        host: Option<String>,
        #[arg(long, requires = "host")]
        port: Option<u16>,
        #[arg(long)]
        notify: bool,
        /// RPIdentity keys for Pair-Verify (defaults to ~/.local/share/ac-dc/rpidentity.json).
        #[arg(long)]
        identity: Option<PathBuf>,
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
            id,
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
                        id,
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
            let keys = keystore::resolve_path(keys, keystore::default_keys_path(), "keys.json");
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
        Cmd::ReceiveKey { out, auto_pair } => {
            let out = out.unwrap_or_else(keystore::default_keys_path);
            tokio::select! {
                r = transfer_ble::receive_key(&out) => r?,
                _ = shutdown() => {
                    tracing::info!("stopping");
                    return Ok(());
                }
            }
            if auto_pair {
                if let Ok(store) = keystore::KeyStore::load(&out) {
                    if let Some(ids_id) = store.ids_device_id {
                        let rp_out = keystore::default_rpidentity_path();
                        println!("Auto-pairing over Companion-Link using IDS device {ids_id}...");
                        let paired = async {
                            let (host, port) =
                                discover::companion_link_target("127.0.0.1", 0).await?;
                            let addr = discover::socket_target(&host, port).await?;
                            let stream = tokio::net::TcpStream::connect(addr).await?;
                            stream.set_nodelay(true).ok();
                            pair_setup::run_pair_setup(stream, Some(&ids_id), || async {
                                Ok("public".to_string())
                            })
                            .await
                        }
                        .await;
                        let paired = keys_saved_context(paired, &out)?;
                        pair_setup::write_identity_json(&rp_out, &paired)?;
                        println!(
                            "Paired with '{}' and saved {}",
                            paired.label,
                            rp_out.display()
                        );
                        println!(
                            "{}",
                            request_clipboard_start(daemon::default_socket()).await
                        );
                    } else {
                        println!(
                            "Next step: run `ac-dc pair --pairing-id <Mac-or-iPhone-IDS-UUID>`"
                        );
                    }
                }
            }
        }
        Cmd::Decrypt { keys, data } => {
            let keys = keystore::resolve_path(keys, keystore::default_keys_path(), "keys.json");
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
                let rpidentity = keystore::resolve_path(
                    None,
                    keystore::default_rpidentity_path(),
                    "rpidentity.json",
                );
                let keys = keystore::resolve_path(keys, keystore::default_keys_path(), "keys.json");
                match load_pull_identity(identity, rpidentity, keys)? {
                    Some(ident) => {
                        tracing::info!(%host, port, "connecting companion-link with Pair-Verify");
                        companion_client::connect(&host, port, ident).await?
                    }
                    None => companion_client::connect_transient(&host, port, "public").await?,
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
            let out = out.unwrap_or_else(keystore::default_rpidentity_path);
            let pairing_id = pairing_id.or_else(|| {
                let kp = keystore::resolve_path(None, keystore::default_keys_path(), "keys.json");
                keystore::KeyStore::load(&kp)
                    .ok()
                    .and_then(|s| s.ids_device_id)
            });
            if pairing_id.is_none() {
                tracing::warn!(
                    "no --pairing-id supplied and keys.json has no ids_device_id; \
                     Mac sharingd may reject clipboard pulls with -6708 unless the UUID is in IDSService.devices"
                );
            }
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
            if out == keystore::default_rpidentity_path() {
                println!(
                    "{}",
                    request_clipboard_start(daemon::default_socket()).await
                );
            }
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
            let keys = keystore::resolve_path(keys, keystore::default_keys_path(), "keys.json");
            let identity = keystore::resolve_path(
                identity,
                keystore::default_rpidentity_path(),
                "rpidentity.json",
            );
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

/// The Pair-Verify identity for `pull`. Only when no identity was asked for and
/// no `rpidentity.json` exists is it `None` (Transient Pair-Setup); a requested
/// or existing identity that cannot be loaded is an error, never a silent
/// downgrade to the public PIN, which authenticates no peer.
fn load_pull_identity(
    explicit: Option<PathBuf>,
    rpidentity: PathBuf,
    keys: PathBuf,
) -> Result<Option<companion::PairingIdentity>> {
    let strict = explicit.is_some() || rpidentity.exists();
    let path = explicit.unwrap_or(if strict { rpidentity } else { keys });
    match companion::PairingIdentity::load(&path) {
        Ok(ident) => Ok(Some(ident)),
        Err(e) if strict => Err(e.context(format!("load RPIdentity {}", path.display()))),
        Err(e) => {
            tracing::warn!(error = %e, "no RPIdentity found; falling back to Transient Pair-Setup (fixedPIN=\"public\")");
            Ok(None)
        }
    }
}

/// Say how to finish pairing when the keys were already written but pairing failed.
fn keys_saved_context<T>(result: Result<T>, keys: &std::path::Path) -> Result<T> {
    use anyhow::Context;
    result.with_context(|| format!("keys saved to {}; retry with `ac-dc pair`", keys.display()))
}

/// Tell the running daemon that credentials now exist: it checks them only
/// when asked, so without this clipboard sync stays `unpaired` until restart.
/// An unreachable daemon is not an error; it picks the keys up on its next start.
async fn request_clipboard_start(socket: PathBuf) -> String {
    match daemon::ctl(socket, daemon::request("clipboard_start")).await {
        Ok(reply) if reply.ok => format!("clipboard sync: {}", reply.message),
        Ok(reply) => format!("clipboard sync not started: {}", reply.message),
        Err(e) => {
            format!("clipboard sync not started ({e:#}); the daemon starts it on its next launch")
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("ac-dc").chain(args.iter().copied()))
    }

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn host_without_port_is_rejected() {
        assert!(parse(&["pull", "--host", "mac.local"]).is_err());
        assert!(parse(&["pair", "--host", "mac.local"]).is_err());
        assert!(parse(&["pull", "--host", "mac.local", "--port", "49152"]).is_ok());
        assert!(parse(&["pair", "--host", "mac.local", "--port", "49152"]).is_ok());
        assert!(parse(&["pull"]).is_ok());
        assert!(parse(&["pair"]).is_ok());
    }

    #[test]
    fn auto_endpoint_override_needs_both_host_and_port() {
        assert!(parse(&["auto", "--port", "1234"]).is_err());
        assert!(parse(&["auto", "--host", "mac.local"]).is_err());
        assert!(parse(&["auto", "--host", "mac.local", "--port", "1234"]).is_ok());
        assert!(parse(&["auto"]).is_ok());
    }

    #[test]
    fn receive_key_auto_pair_can_be_turned_off() {
        let auto_pair = |args: &[&str]| match parse(args).unwrap().cmd {
            Cmd::ReceiveKey { auto_pair, .. } => auto_pair,
            _ => unreachable!(),
        };
        assert!(auto_pair(&["receive-key"]));
        assert!(!auto_pair(&["receive-key", "--auto-pair=false"]));
    }

    const IDENTITY: &str = r#"{"ed_sk":"0101010101010101010101010101010101010101010101010101010101010101","peers":[]}"#;

    #[test]
    fn explicit_unreadable_identity_never_downgrades_to_transient() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.json");
        let err = load_pull_identity(
            Some(missing.clone()),
            dir.path().join("rpidentity.json"),
            dir.path().join("keys.json"),
        )
        .map(|_| ())
        .unwrap_err();
        assert!(format!("{err:#}").contains("missing.json"));

        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "not json").unwrap();
        assert!(
            load_pull_identity(Some(bad), dir.path().join("rp"), dir.path().join("k")).is_err()
        );
    }

    #[test]
    fn corrupt_default_identity_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let rp = dir.path().join("rpidentity.json");
        std::fs::write(&rp, "not json").unwrap();
        assert!(load_pull_identity(None, rp, dir.path().join("keys.json")).is_err());
    }

    #[test]
    fn transient_only_when_no_identity_exists() {
        let dir = tempfile::tempdir().unwrap();
        let keys = dir.path().join("keys.json");
        std::fs::write(&keys, r#"{"keys":[]}"#).unwrap();
        let rp = dir.path().join("rpidentity.json");
        assert!(load_pull_identity(None, rp.clone(), keys.clone())
            .unwrap()
            .is_none());
        std::fs::write(&rp, IDENTITY).unwrap();
        assert!(load_pull_identity(None, rp.clone(), keys.clone())
            .unwrap()
            .is_some());
        assert!(load_pull_identity(Some(rp), dir.path().join("x"), keys)
            .unwrap()
            .is_some());
    }

    #[test]
    fn failed_auto_pair_says_how_to_finish() {
        let failed: Result<()> = Err(anyhow::anyhow!("companion-link discovery timed out"));
        let err = keys_saved_context(failed, std::path::Path::new("/k/keys.json")).unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("/k/keys.json") && text.contains("ac-dc pair"));
        assert!(text.contains("discovery timed out"));
    }

    #[tokio::test]
    async fn pairing_tells_the_running_daemon_to_start_clipboard_sync() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("control.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut line = String::new();
            BufReader::new(read).read_line(&mut line).await.unwrap();
            let reply = r#"{"ok":true,"state":"idle","message":"clipboard sync started"}"#;
            write
                .write_all(format!("{reply}\n").as_bytes())
                .await
                .unwrap();
            line
        });
        let said = request_clipboard_start(socket).await;
        let request: serde_json::Value = serde_json::from_str(&server.await.unwrap()).unwrap();
        assert_eq!(request["op"], "clipboard_start");
        assert_eq!(said, "clipboard sync: clipboard sync started");
    }

    #[tokio::test]
    async fn unreachable_daemon_is_reported_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let said = request_clipboard_start(dir.path().join("absent.sock")).await;
        assert!(said.starts_with("clipboard sync not started"));
    }
}
