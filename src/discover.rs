//! Scoped companion-link discovery. Discovery is a hint; Pair-Verify authenticates peers.
use anyhow::{ensure, Context, Result};
use mdns_sd::{IfKind, ServiceDaemon, ServiceEvent};
use std::{
    net::{IpAddr, SocketAddr, SocketAddrV6},
    time::Duration,
};
const SERVICE: &str = "_companion-link._tcp.local.";
/// Instance name our own Companion-Link server registers (`companion_server`).
pub const OWN_INSTANCE: &str = "ac-dc";
struct Daemon(ServiceDaemon);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.stop_browse(SERVICE);
        let _ = self.0.shutdown();
    }
}
pub fn scope_address(address: IpAddr, iface: &str) -> Result<SocketAddr> {
    match address {
        IpAddr::V6(v) if v.is_unicast_link_local() => {
            let s = std::ffi::CString::new(iface)?;
            let idx = unsafe { libc::if_nametoindex(s.as_ptr()) };
            ensure!(idx != 0, "interface {iface} does not exist");
            Ok(SocketAddr::V6(SocketAddrV6::new(v, 0, 0, idx)))
        }
        _ => Ok(SocketAddr::new(address, 0)),
    }
}
pub async fn socket_target(host: &str, port: u16) -> Result<SocketAddr> {
    if let Some((ip, zone)) = host.rsplit_once('%') {
        let ip: std::net::Ipv6Addr = ip.trim_start_matches('[').parse()?;
        let zone = zone.trim_end_matches(']');
        let index = match zone.parse::<u32>() {
            Ok(i) => i,
            Err(_) => {
                let s = std::ffi::CString::new(zone)?;
                unsafe { libc::if_nametoindex(s.as_ptr()) }
            }
        };
        ensure!(index > 0, "invalid IPv6 scope {zone}");
        return Ok(SocketAddr::V6(SocketAddrV6::new(ip, port, 0, index)));
    }
    tokio::net::lookup_host((host, port))
        .await?
        .next()
        .context("host resolved to no addresses")
}
fn host_string(a: SocketAddr) -> String {
    match a {
        SocketAddr::V6(v) if v.scope_id() != 0 => format!("{}%{}", v.ip(), v.scope_id()),
        _ => a.ip().to_string(),
    }
}
pub async fn companion_link_target(host: &str, port: u16) -> Result<(String, u16)> {
    if port != 0 {
        return Ok((host.into(), port));
    }
    resolve_preferred(None, Duration::from_secs(10)).await
}
fn normalized_address(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_hexdigit())
        .flat_map(char::to_lowercase)
        .collect()
}
/// Try LAN mDNS first (works without `awdl0`), then fall back to `awdl0` if present.
pub async fn resolve_preferred(instance: Option<&str>, timeout: Duration) -> Result<(String, u16)> {
    let lan_budget = timeout.min(Duration::from_secs(4));
    match resolve_on(None, instance, lan_budget).await {
        Ok(target) => Ok(target),
        Err(lan_err) => {
            if std::path::Path::new("/sys/class/net/awdl0").exists() {
                resolve_on(Some("awdl0"), instance, timeout).await
            } else {
                Err(lan_err)
            }
        }
    }
}

/// Whether a resolved service is a pull target. An explicit `instance` wins;
/// otherwise our own advertisement is never a target (another host's service
/// named `ac-dc` is skipped too; select it with an explicit `instance`).
///
/// There is deliberately no BLE-address filter: the source address of a 0x0c
/// advert does not reliably equal the `rpBA` the service publishes (it did not
/// on a live Mac), so comparing them rejected the right device.
/// Pair-Verify authenticates the peer.
fn service_matches(fullname: &str, instance: Option<&str>) -> bool {
    match instance {
        Some(want) => fullname == want,
        None => fullname.strip_suffix(&format!(".{SERVICE}")) != Some(OWN_INSTANCE),
    }
}

/// Off AWDL there is no interface to scope an `fe80::` address with, and an
/// unscoped one cannot be dialed, so such services are skipped.
fn pick_address(mut addrs: Vec<IpAddr>, iface: Option<&str>) -> Result<Option<SocketAddr>> {
    addrs.sort();
    match iface {
        Some(i) => addrs.first().map(|a| scope_address(*a, i)).transpose(),
        None => Ok(addrs
            .into_iter()
            .find(|a| !matches!(a, IpAddr::V6(v) if v.is_unicast_link_local()))
            .map(|a| SocketAddr::new(a, 0))),
    }
}

pub async fn resolve_on(
    iface: Option<&str>,
    instance: Option<&str>,
    timeout: Duration,
) -> Result<(String, u16)> {
    if let Some(i) = iface {
        ensure!(
            std::path::Path::new("/sys/class/net").join(i).exists(),
            "AWDL interface {i} is absent; run awdlctl doctor"
        );
    }
    let daemon = Daemon(ServiceDaemon::new()?);
    if let Some(i) = iface {
        daemon.0.disable_interface(IfKind::All)?;
        daemon.0.enable_interface(i)?;
    }
    let receiver = daemon.0.browse(SERVICE)?;
    let end = tokio::time::Instant::now() + timeout;
    loop {
        let evt = tokio::time::timeout_at(end, receiver.recv_async())
            .await
            .context("companion-link discovery timed out")??;
        if let ServiceEvent::ServiceResolved(info) = evt {
            if !service_matches(info.get_fullname(), instance) {
                continue;
            }
            let addrs: Vec<_> = info.get_addresses().iter().copied().collect();
            if let Some(scoped) = pick_address(addrs, iface)? {
                return Ok((host_string(scoped), info.get_port()));
            }
        }
    }
}
/// Optional management-frame fallback, using a fresh JSONL capture from `awdlctl events`.
/// Require a selected AWDL MAC: BLE and AWDL MACs are not interchangeable.
pub fn from_events(
    path: &std::path::Path,
    peer: &str,
    iface: &str,
    now_ms: u64,
) -> Result<(String, u16)> {
    use std::io::Read;
    let mut data = Vec::new();
    std::fs::File::open(path)?
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut data)?;
    ensure!(data.len() <= 4 * 1024 * 1024, "event capture exceeds 4 MiB");
    let mut target = None;
    for line in data.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let event: serde_json::Value = serde_json::from_slice(line)?;
        let stamp = event["timestamp_ms"].as_u64().unwrap_or(0);
        if stamp > now_ms
            || now_ms - stamp > 30_000
            || event["interface"] != iface
            || normalized_address(event["source"].as_str().unwrap_or(""))
                != normalized_address(peer)
        {
            continue;
        }
        for service in event["frame"]["services"].as_array().into_iter().flatten() {
            if service["record_type"] != 33
                || !service["name"]
                    .as_str()
                    .unwrap_or("")
                    .ends_with("._companion-link._tcp.local")
            {
                continue;
            }
            let port = service["port"]
                .as_u64()
                .filter(|p| *p > 0 && *p <= 65535)
                .context("invalid captured port")? as u16;
            let host = event["host"]
                .as_str()
                .context("missing captured IPv6 address")?;
            let (ip, zone) = host
                .rsplit_once('%')
                .context("captured address missing interface scope")?;
            ensure!(
                zone == iface && ip.parse::<std::net::Ipv6Addr>()?.is_unicast_link_local(),
                "invalid captured endpoint"
            );
            target = Some((host.to_string(), port));
        }
    }
    target.context("no fresh companion-link SRV record for selected AWDL peer")
}
pub async fn run(iface: Option<&str>) -> Result<()> {
    let daemon = Daemon(ServiceDaemon::new()?);
    if let Some(i) = iface {
        daemon.0.disable_interface(IfKind::All)?;
        daemon.0.enable_interface(i)?;
    }
    let rx = daemon.0.browse(SERVICE)?;
    loop {
        if let ServiceEvent::ServiceResolved(i) = rx.recv_async().await? {
            let addresses = i
                .get_addresses()
                .iter()
                .map(|a| {
                    if let Some(iface) = iface {
                        scope_address(*a, iface).map(host_string)
                    } else {
                        Ok(a.to_string())
                    }
                })
                .collect::<Result<Vec<_>>>()?;
            println!(
                "{}",
                serde_json::json!({"instance":i.get_fullname(),"port":i.get_port(),"addresses":addresses,"rpBA":i.get_property_val_str("rpBA")})
            );
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn scope_and_override() {
        assert_eq!(
            companion_link_target("127.0.0.1", 1234).await.unwrap(),
            ("127.0.0.1".into(), 1234)
        );
        let a = socket_target("fe80::1%lo", 1234).await.unwrap();
        assert!(matches!(a,SocketAddr::V6(v) if v.scope_id()>0));
        assert!(socket_target("fe80::1%not-a-real-iface", 1).await.is_err());
    }
    #[test]
    fn normalization() {
        assert_eq!(normalized_address("AA:BB:CC:DD:EE:FF"), "aabbccddeeff");
    }

    const OWN: &str = "ac-dc._companion-link._tcp.local.";
    const MAC: &str = "phone._companion-link._tcp.local.";

    #[test]
    fn never_selects_our_own_advertisement() {
        // Our server publishes no rpBA when it has no BLE address, which the
        // old lenient filter accepted: the pull then dialed itself and failed
        // Pair-Verify with "unknown peer signature".
        assert!(!service_matches(OWN, None));
        assert!(service_matches(MAC, None));
    }

    #[test]
    fn lan_pass_never_returns_an_unscoped_link_local_address() {
        let ll: IpAddr = "fe80::1".parse().unwrap();
        let v4: IpAddr = "192.168.1.9".parse().unwrap();
        assert_eq!(pick_address(vec![ll], None).unwrap(), None);
        assert_eq!(
            pick_address(vec![ll, v4], None).unwrap(),
            Some(SocketAddr::new(v4, 0))
        );
        let scoped = pick_address(vec![ll], Some("lo")).unwrap().unwrap();
        assert!(matches!(scoped, SocketAddr::V6(v) if v.scope_id() > 0));
    }

    #[test]
    fn instance_selection() {
        assert!(service_matches(MAC, Some(MAC)));
        assert!(!service_matches(
            MAC,
            Some("other._companion-link._tcp.local.")
        ));
        // An explicit instance beats the self-exclusion.
        assert!(service_matches(OWN, Some(OWN)));
        // Only the exact instance label is ours, not one that merely starts with it.
        assert!(service_matches(
            "ac-dc.lab._companion-link._tcp.local.",
            None
        ));
    }
}

#[cfg(test)]
mod event_tests {
    use super::*;
    #[test]
    fn fresh_selected_service_only() {
        let path =
            std::env::temp_dir().join(format!("ac-dc-events-{:032x}", rand::random::<u128>()));
        struct Remove(std::path::PathBuf);
        impl Drop for Remove {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _remove = Remove(path.clone());
        let event = serde_json::json!({"timestamp_ms":100_000,"interface":"awdl0","source":"02:11:22:33:44:55","host":"fe80::11:22ff:fe33:4455%awdl0","frame":{"services":[{"name":"phone._companion-link._tcp.local","record_type":33,"port":49152}]}});
        std::fs::write(&path, serde_json::to_vec(&event).unwrap()).unwrap();
        assert_eq!(
            from_events(&path, "02:11:22:33:44:55", "awdl0", 100_001)
                .unwrap()
                .1,
            49152
        );
        assert!(from_events(&path, "02:11:22:33:44:66", "awdl0", 100_001).is_err());
        assert!(from_events(&path, "02:11:22:33:44:55", "awdl0", 140_000).is_err());
        assert!(from_events(&path, "02:11:22:33:44:55", "awdl0", 99_999).is_err());
        assert!(from_events(&path, "02:11:22:33:44:55", "lo", 100_001).is_err());
    }
}
