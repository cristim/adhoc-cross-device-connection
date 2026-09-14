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
use mdns_sd::{IfKind, ServiceDaemon, ServiceEvent};
use std::time::Duration;

const SERVICE: &str = "_companion-link._tcp.local.";

/// The AWDL peer-to-peer Wi-Fi interface Universal Clipboard announces over.
/// It does NOT exist on this host until the OWL/driver bring-up lands
/// (docs/m2-awdl-plan.md) — `awdl_interface_present()` is honest about that.
const AWDL_IFACE: &str = "awdl0";

/// How long an automatic resolve waits before giving up.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(10);

/// Is an `awdl0` interface up on this host right now?
///
/// Today this returns `false` on this machine: `awdl0` does not exist until the
/// AWDL driver work lands (only `lo`/`wlan0` are present). Kernel network
/// interfaces show up as directories under `/sys/class/net/`.
pub fn awdl_interface_present() -> bool {
    std::path::Path::new("/sys/class/net").join(AWDL_IFACE).exists()
}

/// Resolve the companion-link peer's `(host, port)` for `ac-dc pull`.
///
/// Two paths:
///   * **Manual override** — if a non-zero `port` is supplied, trust the
///     caller's `host`/`port` and skip discovery. This keeps `--host/--port`,
///     loopback, and the plain-LAN experiment (`ac-dc discover`) working.
///   * **Automatic** — with `port == 0`, resolve `_companion-link._tcp` scoped
///     to `awdl0`. This is the eventual wake-driven path (a BLE "clipboard
///     available" advert triggers the lookup).
///
/// AWDL-GATED: automatic resolution requires `awdl0`, which does not exist yet;
/// [`discover_over_awdl`] returns a clear "AWDL interface not up yet" error in
/// that case rather than pretending.
pub async fn companion_link_target(host: &str, port: u16) -> Result<(String, u16)> {
    if port != 0 {
        return Ok((host.to_string(), port));
    }
    discover_over_awdl(RESOLVE_TIMEOUT).await
}

/// Browse `_companion-link._tcp` scoped to `awdl0` and return the first
/// resolved `(host, port)`.
///
/// If `awdl0` is absent — the case today — this returns a descriptive error
/// instead of a stub value. When the interface exists, it constrains `mdns-sd`
/// to `awdl0` (IPv6 link-local, mDNS group `ff02::fb`) and waits for a resolve.
///
/// AWDL-GATED: the browse body below has never run on this host (no `awdl0`).
/// It is kept compiled and honest so it's ready the moment the interface comes
/// up — the live experiment described in docs/m2-awdl-plan.md §4.
async fn discover_over_awdl(timeout: Duration) -> Result<(String, u16)> {
    if !awdl_interface_present() {
        anyhow::bail!(
            "AWDL interface not up yet: no `{AWDL_IFACE}` interface exists on this host. \
             Universal Clipboard announces `_companion-link._tcp` over Apple's AWDL \
             peer-to-peer Wi-Fi, which needs the OWL / driver bring-up that has not \
             landed yet (see docs/m2-awdl-plan.md). Until then, pass --host/--port \
             manually, or run `ac-dc discover` to check whether it happens to resolve \
             over the plain LAN."
        );
    }

    let daemon = ServiceDaemon::new().context("starting mDNS daemon")?;
    // Constrain the daemon to awdl0 only: drop every interface, then re-enable
    // awdl0 by name. Without this, mdns-sd would query over wlan0/lo too.
    daemon
        .disable_interface(IfKind::All)
        .context("scoping mDNS: disabling all interfaces")?;
    daemon
        .enable_interface(AWDL_IFACE)
        .with_context(|| format!("scoping mDNS to {AWDL_IFACE}"))?;

    let receiver = daemon.browse(SERVICE).context("browsing companion-link over awdl0")?;
    tracing::info!("browsing {SERVICE} scoped to {AWDL_IFACE} (timeout {timeout:?})");

    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .context("timed out resolving _companion-link._tcp over awdl0")?;
        let evt = tokio::time::timeout(remaining, receiver.recv_async())
            .await
            .context("timed out resolving _companion-link._tcp over awdl0")?
            .context("mDNS receiver closed")?;

        if let ServiceEvent::ServiceResolved(info) = evt {
            let port = info.get_port();
            // TODO(awdl-scope): an awdl0 address is IPv6 link-local and needs
            // the `%awdl0` scope id appended before connect(); mdns-sd hands us
            // a bare address. Prefer a resolved address, else the hostname.
            let host = info
                .get_addresses()
                .iter()
                .map(|a| a.to_string())
                .next()
                .unwrap_or_else(|| info.get_hostname().to_string());
            tracing::info!(%host, port, "resolved companion-link over {AWDL_IFACE}");
            return Ok((host, port));
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn manual_override_passes_host_and_port_through() {
        // A non-zero port means "trust the caller" — no discovery, no awdl0.
        let (host, port) = companion_link_target("192.168.1.42", 49152).await.unwrap();
        assert_eq!(host, "192.168.1.42");
        assert_eq!(port, 49152);
    }

    #[tokio::test]
    async fn automatic_resolve_is_honest_without_awdl() {
        // On this host (and any without the driver work) awdl0 is absent, so an
        // automatic resolve must fail with a clear AWDL message, not hang or
        // fake a target. Guarded so it stays valid should a machine ever have
        // awdl0 up.
        if !awdl_interface_present() {
            let err = companion_link_target("", 0).await.unwrap_err();
            let msg = err.to_string();
            assert!(msg.contains("AWDL"), "expected an AWDL message, got: {msg}");
        }
    }
}
