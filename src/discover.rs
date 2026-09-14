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

/// Resolve the companion-link peer's `(host, port)` for `ac-dc pull`.
///
/// STUB: today this only validates and passes through the CLI-supplied
/// `host`/`port`. Once `awdl0` exists (see `docs/m2-awdl-plan.md`), this is
/// where a `browse`/resolve of `_companion-link._tcp` over that interface will
/// supply the peer's IPv6 link-local address, port, and scope id automatically
/// — triggered by the BLE "clipboard available" wake signal from M1. For now,
/// discovery is manual: run `ac-dc discover` to find the values, then pass them.
pub fn companion_link_target(host: &str, port: u16) -> Result<(String, u16)> {
    if port == 0 {
        anyhow::bail!(
            "no companion-link port: pass --port (auto-discovery over awdl0 is not wired yet; \
             see `ac-dc discover` and docs/m2-awdl-plan.md)"
        );
    }
    Ok((host.to_string(), port))
}

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
