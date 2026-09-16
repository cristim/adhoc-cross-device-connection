use anyhow::Result;
use std::{collections::BTreeMap, net::SocketAddr, path::Path, time::Duration};
#[derive(Clone, Debug, serde::Serialize)]
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
pub async fn browse(iface: &str, identity: &Path, seconds: u64) -> Result<Vec<Peer>> {
    super::identity(identity)?;
    let d = Browse(mdns_sd::ServiceDaemon::new()?);
    d.0.disable_interface(mdns_sd::IfKind::All)?;
    d.0.enable_interface(iface)?;
    let rx = d.0.browse("_airdrop._tcp.local.")?;
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
    if let Ok(state) = crate::health::radio().await {
        if let Some(own) = state["link_local"]
            .as_str()
            .and_then(|s| s.parse::<std::net::IpAddr>().ok())
        {
            candidates.retain(|address, _| address.ip() != own);
        }
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
                instance,
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
