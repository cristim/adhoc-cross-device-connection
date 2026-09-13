//! mDNS-SD discovery of `_companion-link._tcp` — the service Handoff /
//! Universal Clipboard use for the content transfer.
//!
//! This is the ONE part of M2 that is live-testable right now, and it's the
//! critical experiment for the whole content-pull path: normally the service is
//! announced over Apple's AWDL peer-to-peer Wi-Fi, which this machine's
//! `brcmfmac` cannot do (no monitor mode). If `discover` finds your iPhone/Mac
//! over your ordinary Wi-Fi/Ethernet LAN, M2 is viable here; if nothing ever
//! resolves while the devices are awake and nearby, the transport is AWDL-only
//! and M2 becomes a kernel-driver problem.
//!
//! The TXT record carries `rpBA` (a rotating BLE-address string), `rpAD` (a
//! SipHash auth tag over rpBA and the device IRK — used to confirm same Apple
//! ID), and `rpVr` (version).

use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceEvent};

const SERVICE: &str = "_companion-link._tcp.local.";

pub async fn run() -> Result<()> {
    let daemon = ServiceDaemon::new().context("starting mDNS daemon")?;
    let receiver = daemon.browse(SERVICE).context("browsing companion-link")?;

    tracing::info!("browsing {SERVICE} (Ctrl-C to stop)");
    println!("# waiting for _companion-link._tcp instances on the local network...");

    loop {
        match receiver.recv_async().await {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                println!("resolved: {}", info.get_fullname());
                println!("  host:  {}", info.get_hostname());
                println!("  port:  {}", info.get_port());
                let addrs: Vec<String> = info.get_addresses().iter().map(|a| a.to_string()).collect();
                println!("  addrs: {}", if addrs.is_empty() { "(none yet)".into() } else { addrs.join(", ") });
                for prop in info.get_properties().iter() {
                    let val = prop.val_str();
                    println!("  txt:   {}={}", prop.key(), val);
                }
            }
            Ok(ServiceEvent::ServiceFound(_, name)) => {
                tracing::debug!(%name, "service found (resolving)");
            }
            Ok(ServiceEvent::ServiceRemoved(_, name)) => {
                println!("removed: {name}");
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(error = %e, "mDNS receiver closed");
                break;
            }
        }
    }
    Ok(())
}
