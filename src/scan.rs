//! BLE scanning via BlueZ (D-Bus). Listens for Apple manufacturer data, tries
//! to parse it as a Handoff/Universal-Clipboard advertisement, and (in scan
//! mode) decrypts it with every key we hold until one authenticates.
//!
//! Two modes share the discovery loop:
//!   * `run()` – decrypt-and-report (needs keys).
//!   * `run_capture()` – print each raw Handoff advert as hex, ready to paste
//!     into `ac-dc decrypt --data <hex>`. Needs no keys; used to grab a
//!     validation packet from your own devices.

use anyhow::{Context, Result};
use bluer::{Adapter, AdapterEvent, DeviceEvent, DeviceProperty};
use futures::{pin_mut, StreamExt};
use std::collections::HashMap;

use crate::advert::{self, HandoffBle, HandoffPayload};
use crate::gcm;
use crate::keystore::{HandoffKey, KeyStore};

pub struct Scanner {
    adapter: Adapter,
    keys: Vec<HandoffKey>,
    capture: bool,
}

impl Scanner {
    pub async fn new(store: KeyStore) -> Result<Self> {
        Self::build(store.keys, false).await
    }

    /// Keyless capture mode: just surface raw Handoff adverts as hex.
    pub async fn new_capture() -> Result<Self> {
        Self::build(Vec::new(), true).await
    }

    async fn build(keys: Vec<HandoffKey>, capture: bool) -> Result<Self> {
        let session = bluer::Session::new().await.context("connecting to bluetoothd")?;
        let adapter = session.default_adapter().await.context("no BLE adapter")?;
        adapter.set_powered(true).await.context("powering on adapter")?;
        tracing::info!(adapter = %adapter.name(), "using BLE adapter");
        Ok(Scanner { adapter, keys, capture })
    }

    pub async fn run(&self) -> Result<()> {
        tracing::info!("scanning for Apple Continuity advertisements (Ctrl-C to stop)");
        self.discover_loop().await
    }

    pub async fn run_capture(&self) -> Result<()> {
        tracing::info!("capture mode: printing raw Handoff adverts (Ctrl-C to stop)");
        println!("# paste a line into: ac-dc decrypt --keys keys.json --data <hex>");
        self.discover_loop().await
    }

    async fn discover_loop(&self) -> Result<()> {
        let discover = self.adapter.discover_devices().await?;
        pin_mut!(discover);

        while let Some(evt) = discover.next().await {
            let AdapterEvent::DeviceAdded(addr) = evt else {
                continue;
            };
            let device = match self.adapter.device(addr) {
                Ok(d) => d,
                Err(_) => continue,
            };
            if let Ok(Some(md)) = device.manufacturer_data().await {
                self.worker().handle(addr, &md);
            }
            let events = match device.events().await {
                Ok(e) => e,
                Err(_) => continue,
            };
            // One lightweight follower per device so several phones/Macs can
            // announce concurrently.
            let worker = self.worker();
            tokio::spawn(async move {
                pin_mut!(events);
                while let Some(ev) = events.next().await {
                    if let DeviceEvent::PropertyChanged(DeviceProperty::ManufacturerData(md)) = ev {
                        worker.handle(addr, &md);
                    }
                }
            });
        }
        Ok(())
    }

    fn worker(&self) -> Worker {
        Worker {
            keys: self.keys.clone(),
            capture: self.capture,
        }
    }
}

#[derive(Clone)]
struct Worker {
    keys: Vec<HandoffKey>,
    capture: bool,
}

impl Worker {
    fn handle(&self, addr: bluer::Address, md: &HashMap<u16, Vec<u8>>) {
        let Some(data) = advert::apple_manufacturer_data(md) else {
            return;
        };
        let Some(ble) = HandoffBle::parse(data) else {
            return;
        };
        tracing::debug!(%addr, status = ble.status, ctr = ?ble.counter_iv, "handoff advert");

        if self.capture {
            self.capture_line(addr, data, &ble);
            return;
        }

        // Try each key until the 1-byte tag authenticates.
        for k in &self.keys {
            let plain = match gcm::open_truncated(
                &k.key,
                &ble.counter_iv,
                &[ble.status],
                &ble.ciphertext,
                &ble.tag,
            ) {
                Ok(Some(plain)) => plain,
                Ok(None) => continue,
                Err(e) => {
                    tracing::error!(key = %k.id, error = %e, "skipping key");
                    continue;
                }
            };
            if let Some(payload) = HandoffPayload::parse(&plain) {
                self.report(addr, &k.id, &payload);
                return;
            }
        }
    }

    /// Print a raw advert for offline validation. We emit the normalized
    /// `0c ..` TLV (what `decrypt --data` and `HandoffBle::parse` accept),
    /// plus the decoded framing so byte-order assumptions can be eyeballed.
    fn capture_line(&self, addr: bluer::Address, raw: &[u8], ble: &HandoffBle) {
        let mut tlv = Vec::with_capacity(2 + ble.ciphertext.len() + 4);
        let len = (1 + 2 + 1 + ble.ciphertext.len()) as u8; // status+iv+tag+ct
        tlv.push(0x0c);
        tlv.push(len);
        tlv.push(ble.status);
        tlv.extend_from_slice(&ble.counter_iv);
        tlv.extend_from_slice(&ble.tag);
        tlv.extend_from_slice(&ble.ciphertext);
        println!(
            "{addr}  {tlv}   # raw_mfg={raw} status={status:#04x} iv={iv} tag={tag} ct={ct}",
            addr = addr,
            tlv = hex::encode(&tlv),
            raw = hex::encode(raw),
            status = ble.status,
            iv = hex::encode(ble.counter_iv),
            tag = hex::encode(ble.tag),
            ct = hex::encode(&ble.ciphertext),
        );
    }

    fn report(&self, addr: bluer::Address, key_id: &str, p: &HandoffPayload) {
        let clip = if p.flags.clipboard_available() {
            "CLIPBOARD AVAILABLE"
        } else {
            "(no clipboard flag)"
        };
        tracing::info!(
            %addr,
            key = key_id,
            activity_hash = hex::encode(p.activity_hash),
            url = p.flags.has_url(),
            "{clip}"
        );
        if p.flags.clipboard_available() {
            println!(
                "[{}] Universal Clipboard: a copy is available on a nearby Apple device (key {})",
                addr, key_id
            );
        }
    }
}
