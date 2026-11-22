use anyhow::{Context, Result};
use std::{collections::BTreeMap, net::SocketAddr, path::Path, time::Duration};
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Peer {
    pub name: String,
    pub instance: String,
    pub address: SocketAddr,
}
struct Browse(mdns_sd::ServiceDaemon);
impl Drop for Browse {
    fn drop(&mut self) {
        let _ = self.0.shutdown();
    }
}
const MAX_INSTANCE_LEN: usize = 128;

pub(crate) fn sanitize_instance(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_INSTANCE_LEN)
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "Airdrop-compatible".to_string()
    } else {
        trimmed.to_string()
    }
}

pub async fn browse(iface: &str, identity: &Path, seconds: u64) -> Result<Vec<Peer>> {
    super::identity(identity).context("load Pair-Verify identity")?;
    let d = Browse(mdns_sd::ServiceDaemon::new().context("create mDNS browser")?);
    d.0.disable_interface(mdns_sd::IfKind::All)
        .context("disable mDNS interfaces")?;
    d.0.enable_interface(iface)
        .with_context(|| format!("enable mDNS on {iface}"))?;
    let rx =
        d.0.browse("_airdrop._tcp.local.")
            .context("browse _airdrop._tcp.local")?;
    let end = tokio::time::Instant::now() + Duration::from_secs(seconds);
    let mut candidates = BTreeMap::new();
    while let Ok(Ok(event)) = tokio::time::timeout_at(end, rx.recv_async()).await {
        if let mdns_sd::ServiceEvent::ServiceResolved(info) = event {
            for ip in info.get_addresses() {
                if !matches!(ip,std::net::IpAddr::V6(v) if v.is_unicast_link_local()) {
                    continue;
                }
                let mut address = crate::discover::scope_address(*ip, iface)?;
                address.set_port(info.get_port());
                candidates.insert(address, info.get_fullname().to_string());
            }
        }
    }
    // Firmware service records provide endpoints even when peer mDNS is silent.
    if let Ok(bytes) = std::fs::read("/run/brcmfmac-awdl/airdrop-peers.json") {
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs();
            if value["timestamp"]
                .as_u64()
                .is_some_and(|t| t <= now && now - t <= 30)
            {
                for p in value["peers"].as_array().into_iter().flatten() {
                    if let (Some(host), Some(port)) = (
                        p["host"].as_str(),
                        p["port"].as_u64().filter(|n| *n > 0 && *n <= 65535),
                    ) {
                        if let Ok(ip) = host.parse::<std::net::Ipv6Addr>() {
                            if ip.is_unicast_link_local() {
                                let mut a = crate::discover::scope_address(ip.into(), iface)?;
                                a.set_port(port as u16);
                                candidates.insert(
                                    a,
                                    p["instance"]
                                        .as_str()
                                        .unwrap_or("Airdrop-compatible")
                                        .into(),
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    // The AWDL driver can decode mDNS service records from management frames
    // even when a userspace mDNS socket does not receive the multicast packet.
    // Use those decoded SRV records as a fallback for Linux AWDL adapters.
    // Our own announcement must not count as a peer, or it suppresses the fallback.
    let own_ip = own_link_local().await;
    let own_id = super::service_id(identity).ok();
    if needs_event_fallback(&mut candidates, own_ip, own_id.as_deref()) {
        let lines = if unsafe { libc::geteuid() } == 0 {
            let output = tokio::process::Command::new("/usr/bin/awdlctl")
                .args(["events", "--seconds", &seconds.to_string()])
                .output()
                .await
                .context("capture AWDL discovery events")?;
            if output.status.success() {
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .map(String::from)
                    .collect()
            } else {
                tracing::debug!(status = ?output.status, "AWDL event fallback unavailable");
                Vec::new()
            }
        } else {
            events_via_broker(Path::new(crate::radio_daemon::DEFAULT_SOCKET), seconds).await?
        };
        for line in lines {
            let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            let Some(host) = event["host"].as_str() else {
                continue;
            };
            let Ok(ip) = host
                .split('%')
                .next()
                .unwrap_or(host)
                .parse::<std::net::Ipv6Addr>()
            else {
                continue;
            };
            if !ip.is_unicast_link_local() {
                continue;
            }
            for service in event["frame"]["services"].as_array().into_iter().flatten() {
                if service["name"]
                    .as_str()
                    .is_none_or(|name| !name.ends_with("._airdrop._tcp.local"))
                {
                    continue;
                }
                let Some(port) = service["port"].as_u64().filter(|p| *p > 0 && *p <= 65535) else {
                    continue;
                };
                let mut address = crate::discover::scope_address(ip.into(), iface)?;
                address.set_port(port as u16);
                candidates.insert(
                    address,
                    service["name"]
                        .as_str()
                        .unwrap_or("Airdrop-compatible")
                        .into(),
                );
            }
        }
        drop_own(&mut candidates, own_ip, own_id.as_deref());
        tracing::debug!(
            count = candidates.len(),
            "AWDL event fallback yielded candidates"
        );
    }
    let mut work = tokio::task::JoinSet::new();
    for (address, instance) in candidates.into_iter().take(32) {
        let id = identity.to_owned();
        work.spawn(async move {
            let name = tokio::time::timeout(
                Duration::from_secs(8),
                super::send::Client::new(address, &id)?.discover(),
            )
            .await??;
            Ok::<_, anyhow::Error>(Peer {
                name,
                instance: sanitize_instance(&instance),
                address,
            })
        });
    }
    let mut peers = Vec::new();
    while let Some(result) = work.join_next().await {
        match result {
            Ok(Ok(peer)) => peers.push(peer),
            Ok(Err(error)) => {
                tracing::debug!(error=%format!("{error:#}"),"Airdrop-compatible candidate unavailable")
            }
            Err(error) => tracing::warn!(%error,"Airdrop-compatible discovery task failed"),
        }
    }
    peers.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(peers)
}

async fn own_link_local() -> Option<std::net::IpAddr> {
    let state = crate::health::radio().await.ok()?;
    state["link_local"].as_str()?.parse().ok()
}

/// Drop our own announcement first, then decide: it must not count as a peer.
fn needs_event_fallback(
    candidates: &mut BTreeMap<SocketAddr, String>,
    own_ip: Option<std::net::IpAddr>,
    own_id: Option<&str>,
) -> bool {
    drop_own(candidates, own_ip, own_id);
    candidates.is_empty()
}

fn drop_own(
    candidates: &mut BTreeMap<SocketAddr, String>,
    own_ip: Option<std::net::IpAddr>,
    own_id: Option<&str>,
) {
    if let Some(own) = own_ip {
        candidates.retain(|address, _| address.ip() != own);
    }
    if let Some(id) = own_id {
        candidates.retain(|_, inst| !is_own_instance(inst, id));
    }
}

async fn events_via_broker(socket: &Path, seconds: u64) -> Result<Vec<String>> {
    crate::radio_daemon::fetch_events(socket, seconds)
        .await
        .context("AWDL event fallback via radio daemon")
}

fn is_own_instance(instance: &str, own_id: &str) -> bool {
    instance == own_id || instance.starts_with(&format!("{own_id}."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_instance_strips_controls_and_bounds_length() {
        assert_eq!(sanitize_instance("  \x00\n\t  "), "Airdrop-compatible");
        assert_eq!(sanitize_instance("Mac\nBook\tPro"), "MacBookPro");
        let long = "a".repeat(300);
        assert_eq!(sanitize_instance(&long).len(), MAX_INSTANCE_LEN);
    }

    #[test]
    fn own_announcement_alone_leaves_no_candidates() {
        let own_addr: SocketAddr = "[fe80::1%2]:8771".parse().unwrap();
        let peer_addr: SocketAddr = "[fe80::2%2]:8771".parse().unwrap();
        let mut by_ip = BTreeMap::from([(own_addr, "whatever".to_string())]);
        drop_own(&mut by_ip, Some(own_addr.ip()), None);
        assert!(by_ip.is_empty());
        let mut by_id = BTreeMap::from([(peer_addr, "aabbccddeeff._airdrop._tcp.local.".into())]);
        drop_own(&mut by_id, None, Some("aabbccddeeff"));
        assert!(by_id.is_empty());
        let mut other = BTreeMap::from([(peer_addr, "112233445566._airdrop._tcp.local.".into())]);
        drop_own(&mut other, Some(own_addr.ip()), Some("aabbccddeeff"));
        assert_eq!(other.len(), 1);
    }

    #[test]
    fn only_our_own_announcement_still_triggers_the_event_fallback() {
        let own_addr: SocketAddr = "[fe80::1%2]:8771".parse().unwrap();
        let peer_addr: SocketAddr = "[fe80::2%2]:8771".parse().unwrap();
        let mut only_us = BTreeMap::from([(own_addr, "us".to_string())]);
        assert!(needs_event_fallback(
            &mut only_us,
            Some(own_addr.ip()),
            None
        ));
        let mut with_peer = BTreeMap::from([
            (own_addr, "us".to_string()),
            (peer_addr, "peer".to_string()),
        ]);
        assert!(!needs_event_fallback(
            &mut with_peer,
            Some(own_addr.ip()),
            None
        ));
    }

    #[tokio::test]
    async fn broker_failure_is_an_error_not_an_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        let err = events_via_broker(&dir.path().join("absent.sock"), 1)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("AWDL event fallback via radio daemon"));
    }

    #[test]
    fn filters_own_service_id_instances() {
        let own = "aabbccddeeff";
        assert!(super::is_own_instance(own, own));
        assert!(super::is_own_instance(
            "aabbccddeeff._airdrop._tcp.local.",
            own
        ));
        assert!(!super::is_own_instance(
            "112233445566._airdrop._tcp.local.",
            own
        ));
    }
}
