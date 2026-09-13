//! handoff-clip: receive Apple Universal Clipboard / Handoff BLE announcements
//! on Linux, using encryption keys exported from a macOS install signed into
//! the same Apple ID.
//!
//! This first milestone covers DISCOVERY + DECRYPTION of the BLE advertisement:
//! it tells you, on Linux, the moment your iPhone or Mac copies something. The
//! subsequent milestones (pulling the actual clipboard content over the
//! companion-link service, then the reverse direction) are tracked in the
//! README roadmap.

mod advert;
mod gcm;
mod keystore;
mod scan;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "handoff-clip", version, about)]
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
    /// Decrypt a single advertisement supplied as a hex string (offline test).
    Decrypt {
        #[arg(short, long)]
        keys: PathBuf,
        /// Manufacturer data hex, e.g. "0c0e082a0099dead...".
        #[arg(short, long)]
        data: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "handoff_clip=info".into()),
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
                ) {
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
    }
    Ok(())
}
