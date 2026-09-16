# Omdrop ideas and provenance

Reviewed plugin commit `daee7a3ed9620b5d1bad277f731fdf9eabc38d53` from
[brentkearney/omdrop-plugin](https://github.com/brentkearney/omdrop-plugin) (MIT).
The Rust receiver implements the documented wire formats; it does not execute
Omdrop's Python or shell tools.

| Idea | Implementation |
| --- | --- |
| Report observed state and unknown transport separately | `health.rs`, `awdlctl status/doctor/probe` |
| Separate privileged radio control and user application | standalone `brcmfmac-awdl` repository, JSON CLI |
| Bounded radio use and Wi-Fi-band restoration | `awdlctl window`, persisted connection-UUID journal |
| Peer registration and table inspection | `awdlctl peer`, `iovar awdl_peer_op`, `diagnose` |
| Firmware counter/election/advertiser decoding | driver `diagnostics.rs` |
| Management-frame endpoint discovery | driver `frames.rs`, `events`; application event fallback |
| Use observed channel schedules | `awdlctl schedule --frame`, explicit window schedule input |
| BlueZ advertisement lifecycle | `advertise.rs`, bounded duration and signal cleanup |
| Clipboard ownership beyond service lifetime | `clipboard_out.rs`, separate user unit |
| Shared-link/file-to-clipboard workflow | Rust `airdrop/` receiver |
| Current Airdrop-compatible chunked requests, DVZip, Ask-only links | `airdrop/http.rs`, `archive.rs`, `mod.rs` |

The radio-side protocol work comes from
[brentkearney/omdrop-awdl](https://github.com/brentkearney/omdrop-awdl) and earlier
[andreanicassio/brcmfmac-awdl](https://github.com/andreanicassio/brcmfmac-awdl).
GPL-derived driver tooling stays in the separate GPL repository.

Two upstream findings require careful interpretation: an iPhone accepted outbound
TCP/TLS/Discover despite file sending remaining incomplete; and successful
receiving with infrastructure Wi-Fi on 2.4 GHz still used mostly 5 GHz AWDL
slots. Neither finding establishes this project's BCM4378 interoperability.

Prior Continuity references remain relevant:
[handoff-ble-viewer](https://github.com/seemoo-lab/handoff-ble-viewer),
[handoff-authentication-swift](https://github.com/seemoo-lab/handoff-authentication-swift),
[apple-continuity-tools](https://github.com/seemoo-lab/apple-continuity-tools), and
[openwifipass](https://github.com/seemoo-lab/openwifipass).

## Airdrop-compatible sending and native UI

The Rust sender independently implements the Discover/Ask/Upload wire contract
observed in [OpenDrop](https://github.com/seemoo-lab/opendrop): self-signed
Everyone-mode TLS, binary-plist requests, regular-file metadata, and DVZip/CPIO
over chunked HTTP. OpenDrop is a research reference, not a runtime dependency;
its GPL Python implementation has not been incorporated into this MIT crate.
The native GTK UI is Rust. Firmware template/packet work remains in the GPL
driver repository.

The OPACK corrections cross-check [pyatv](https://github.com/postlund/pyatv/blob/master/pyatv/support/opack.py)'s
protocol encoding. See the separate research reports for pinned clipboard and
Sapporo references and the distinction between source evidence and hardware tests.


## Additional implementation references (2026-09-16)

[OWL, OpenDrop and LocalSend review](owl-opendrop-localsend-research.md) records
source versions, connection lifetime observations and license boundaries.
LocalSend's official Rust core is a possible future LAN transport reference;
this change adds no LocalSend dependency or protocol. Streaming, cancellation
and approval handling remain independently implemented Rust.

The 0.2.2 sender replaces the pooled reqwest client with independently written
Rust using Hyper's connection API and tokio-openssl. This follows OpenDrop's
observable protocol sequence and choice of TLS library; no GPL Python code is
incorporated. See [the focused sender comparison](opendrop-sender-2026-09-17.md).
