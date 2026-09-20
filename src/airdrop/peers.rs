use anyhow::{Context, Result};
use std::{collections::BTreeMap, net::SocketAddr, path::Path, time::Duration};
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Peer {
    pub name: String,
    pub instance: String,
    pub address: SocketAddr,
}
/// `awdlctl` writes the service records it decoded from AWDL management frames
/// here, refreshing the file while a discoverable window runs.
const FIRMWARE_PEERS: &str = "/run/brcmfmac-awdl/airdrop-peers.json";
/// Records older than this are refused, so a stale file cannot resurrect a peer
/// that has since gone away.
const FIRMWARE_MAX_AGE: u64 = 30;

/// Endpoints from one `awdlctl` service-record file, empty when the file is older
/// than [`FIRMWARE_MAX_AGE`]. `now` is unix seconds.
fn firmware_peers(bytes: &[u8], now: u64, iface: &str) -> Result<Vec<(SocketAddr, String)>> {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return Ok(Vec::new());
    };
    if !value["timestamp"]
        .as_u64()
        .is_some_and(|t| t <= now && now - t <= FIRMWARE_MAX_AGE)
    {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    for p in value["peers"].as_array().into_iter().flatten() {
        let (Some(host), Some(port)) = (
            p["host"].as_str(),
            p["port"].as_u64().filter(|n| *n > 0 && *n <= 65535),
        ) else {
            continue;
        };
        let Ok(ip) = host.parse::<std::net::Ipv6Addr>() else {
            continue;
        };
        if !ip.is_unicast_link_local() {
            continue;
        }
        let mut a = crate::discover::scope_address(ip.into(), iface)?;
        a.set_port(port as u16);
        found.push((
            a,
            p["instance"]
                .as_str()
                .unwrap_or("Airdrop-compatible")
                .into(),
        ));
    }
    Ok(found)
}

/// Poll `path` until it holds fresh records or `deadline` passes. `awdlctl` writes
/// the file only once it has decoded service records, so the first read routinely
/// comes too early; a single read made discovery succeed or fail by timing.
async fn firmware_peers_within(
    path: &std::path::Path,
    iface: &str,
    deadline: tokio::time::Instant,
) -> Result<Vec<(SocketAddr, String)>> {
    loop {
        if let Ok(bytes) = std::fs::read(path) {
            let found = firmware_peers(&bytes, unix_now()?, iface)?;
            if !found.is_empty() {
                return Ok(found);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(Vec::new());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn unix_now() -> Result<u64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs())
}

struct Browse(mdns_sd::ServiceDaemon);
impl Drop for Browse {
    fn drop(&mut self) {
        let _ = self.0.shutdown();
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
    // The `awdlctl discoverable` child refreshes the file only once it has decoded
    // service records, which routinely lands after the mDNS window closes: reading
    // once made discovery a race that returned no peers whenever it read early.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    candidates.extend(
        firmware_peers_within(std::path::Path::new(FIRMWARE_PEERS), iface, deadline).await?,
    );
    // The AWDL driver can decode mDNS service records from management frames
    // even when a userspace mDNS socket does not receive the multicast packet.
    // Use those decoded SRV records as a fallback for Linux AWDL adapters.
    if candidates.is_empty() {
        let output = tokio::process::Command::new("/usr/bin/awdlctl")
            .args(["events", "--seconds", &seconds.to_string()])
            .output()
            .await
            .context("capture AWDL discovery events")?;
        if output.status.success() {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
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
                    let Some(port) = service["port"].as_u64().filter(|p| *p > 0 && *p <= 65535)
                    else {
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
            tracing::debug!(
                count = candidates.len(),
                "AWDL event fallback yielded candidates"
            );
        } else {
            tracing::debug!(status = ?output.status, "AWDL event fallback unavailable");
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

#[cfg(test)]
mod tests {
    use super::*;

    fn file(timestamp: u64) -> Vec<u8> {
        format!(
            r#"{{"peers":[{{"host":"fe80::c897:44ff:fe50:6963","instance":"a._airdrop._tcp.local","port":8770}}],"timestamp":{timestamp}}}"#
        )
        .into_bytes()
    }

    // `lo` stands in for awdl0: scope_address only needs an interface that exists.
    const IFACE: &str = "lo";

    // The race the single read lost: the file is stale when discovery first looks
    // and only becomes fresh afterwards. Pre-fix this returned no peers.
    #[tokio::test]
    async fn a_file_that_turns_fresh_after_the_first_read_is_still_picked_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("airdrop-peers.json");
        std::fs::write(&path, file(unix_now().unwrap() - FIRMWARE_MAX_AGE - 60)).unwrap();

        let writer = {
            let path = path.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(700)).await;
                std::fs::write(&path, file(unix_now().unwrap())).unwrap();
            })
        };

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let found = firmware_peers_within(&path, IFACE, deadline).await.unwrap();
        writer.await.unwrap();

        assert_eq!(found.len(), 1, "the refreshed file was never picked up");
        assert_eq!(found[0].0.port(), 8770);
    }

    #[tokio::test]
    async fn a_file_that_never_refreshes_gives_up_at_the_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("airdrop-peers.json");
        std::fs::write(&path, file(unix_now().unwrap() - FIRMWARE_MAX_AGE - 60)).unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(600);
        assert!(firmware_peers_within(&path, IFACE, deadline)
            .await
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_fresh_service_record_file_yields_its_peer() {
        let now = 1_789_944_322;
        let found = firmware_peers(&file(now - 5), now, IFACE).unwrap();
        assert_eq!(found.len(), 1, "fresh file was not accepted");
        assert_eq!(found[0].0.port(), 8770);
        assert_eq!(found[0].1, "a._airdrop._tcp.local");
    }

    #[test]
    fn a_stale_file_cannot_resurrect_a_peer_that_has_gone() {
        let now = 1_789_944_322;
        let stale = now - FIRMWARE_MAX_AGE - 1;
        assert!(firmware_peers(&file(stale), now, IFACE).unwrap().is_empty());
    }

    #[test]
    fn a_timestamp_from_the_future_is_refused() {
        let now = 1_789_944_322;
        assert!(firmware_peers(&file(now + 60), now, IFACE)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn malformed_or_routable_records_are_skipped_without_failing() {
        let now = 1_789_944_322;
        assert!(firmware_peers(b"not json", now, IFACE).unwrap().is_empty());
        let routable = format!(
            r#"{{"peers":[{{"host":"2001:db8::1","instance":"a","port":8770}}],"timestamp":{now}}}"#
        );
        assert!(firmware_peers(routable.as_bytes(), now, IFACE)
            .unwrap()
            .is_empty());
        let zero_port = format!(
            r#"{{"peers":[{{"host":"fe80::1","instance":"a","port":0}}],"timestamp":{now}}}"#
        );
        assert!(firmware_peers(zero_port.as_bytes(), now, IFACE)
            .unwrap()
            .is_empty());
    }
}
