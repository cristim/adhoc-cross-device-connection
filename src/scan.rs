//! BLE scanning via BlueZ (D-Bus). Listens for Apple manufacturer data, tries
//! to parse it as a Handoff/Universal-Clipboard advertisement, and decrypts it
//! with every key we hold until one authenticates.

use anyhow::{Context, Result};
use bluer::{Adapter, AdapterEvent, DeviceEvent, DeviceProperty};
use futures::{pin_mut, StreamExt};
use std::collections::HashMap;

use crate::advert::{self, HandoffBle, HandoffPayload};
use crate::gcm;
use crate::keystore::KeyStore;

pub struct Scanner {
    adapter: Adapter,
    store: KeyStore,
}

impl Scanner {
    pub async fn new(store: KeyStore) -> Result<Self> {
        let session = bluer::Session::new().await.context("connecting to bluetoothd")?;
        let adapter = session.default_adapter().await.context("no BLE adapter")?;
        adapter.set_powered(true).await.context("powering on adapter")?;
        tracing::info!(adapter = %adapter.name(), "using BLE adapter");
        Ok(Scanner { adapter, store })
    }

    pub async fn run(&self) -> Result<()> {
        let discover = self.adapter.discover_devices().await?;
        pin_mut!(discover);

        tracing::info!("scanning for Apple Continuity advertisements (Ctrl-C to stop)");
        while let Some(evt) = discover.next().await {
            let AdapterEvent::DeviceAdded(addr) = evt else {
                continue;
            };
            let device = match self.adapter.device(addr) {
                Ok(d) => d,
                Err(_) => continue,
            };
            // Inspect current manufacturer data, then follow changes.
            if let Ok(Some(md)) = device.manufacturer_data().await {
                self.handle(addr, &md);
            }
            let store_empty = self.store.is_empty();
            let events = match device.events().await {
                Ok(e) => e,
                Err(_) => continue,
            };
            // Spawn a lightweight follower per device so multiple phones/Macs
            // can announce concurrently.
            let this = self.clone_lite();
            tokio::spawn(async move {
                pin_mut!(events);
                while let Some(ev) = events.next().await {
                    if let DeviceEvent::PropertyChanged(DeviceProperty::ManufacturerData(md)) = ev {
                        this.handle(addr, &md);
                        if store_empty {
                            // Nothing to decrypt with; still useful to see hits.
                        }
                    }
                }
            });
        }
        Ok(())
    }

    /// A cheap clone that shares the keys by value (keys are small).
    fn clone_lite(&self) -> ScannerLite {
        ScannerLite {
            keys: self.store.keys.clone(),
        }
    }

    fn handle(&self, addr: bluer::Address, md: &HashMap<u16, Vec<u8>>) {
        ScannerLite {
            keys: self.store.keys.clone(),
        }
        .handle(addr, md);
    }
}

#[derive(Clone)]
struct ScannerLite {
    keys: Vec<crate::keystore::HandoffKey>,
}

impl ScannerLite {
    fn handle(&self, addr: bluer::Address, md: &HashMap<u16, Vec<u8>>) {
        let Some(data) = advert::apple_manufacturer_data(md) else {
            return;
        };
        let Some(ble) = HandoffBle::parse(data) else {
            return;
        };
        tracing::debug!(%addr, status = ble.status, ctr = ?ble.counter_iv, "handoff advert");

        // Try each key until the 1-byte tag authenticates.
        for k in &self.keys {
            if let Some(plain) = gcm::open_truncated(
                &k.key,
                &ble.counter_iv,
                &[ble.status],
                &ble.ciphertext,
                &ble.tag,
            ) {
                if let Some(payload) = HandoffPayload::parse(&plain) {
                    self.report(addr, &k.id, &payload);
                    return;
                }
            }
        }
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
