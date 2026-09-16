# Architecture

## Repository boundary

`ac-dc` owns BLE, same-account authentication, companion-link, Airdrop-compatible receiving,
and the user clipboard. `brcmfmac-awdl` owns the GPL kernel patches and Rust
`awdlctl` radio tools. The boundary is a versioned CLI/JSON contract, not a Cargo
path dependency or a privileged script inside the user's checkout.

`awdlctl status --json` currently returns `schema_version` (1), `interface`, `present`, `link_up`,
`link_local`, `ready`, `reason`, and nullable `tx_proven`/`rx_proven`. Ready means
an up link and usable IPv6 address. Unmeasured transport stays null. For actual
reachability use the separate outbound TCP probe. `ac-dc` does not infer success
from firmware counters or a process being alive.

The CLI is initially v0.1; consumers should ignore additional JSON fields. A
breaking field change requires updating both repositories and their documentation.

## Universal Clipboard path

1. BlueZ supplies Handoff advertisements; parser/decryption identify copy hints.
2. A bounded event channel connects scan to `auto`, which pulls one item at a time.
3. Read radio state; match mDNS `rpBA` to the BLE event or use an explicit instance.
4. Use a scoped IPv6 socket. Optional manual pull can fall back to fresh decoded
   management-frame SRV records for an explicitly selected AWDL peer.
5. Pair-Verify must authenticate a known Ed25519 peer before content exchange.
6. Decode the pasteboard and select a text/image representation for Wayland.

The whole automatic transaction has a deadline. TCP and Pair-Verify also have
separate deadlines. SIGINT/SIGTERM cancels the scanner/pull. Individual failures
are logged; the process continues waiting for the next copy.

The clipboard owner must outlive a short-lived service: under systemd, a private
0600 input file supplies a separate `wl-copy --foreground` unit. The file is
unlinked after systemd reports exec readiness. Outside systemd, the normal
forking wl-copy path is used with detached output descriptors.

## Airdrop-compatible path

A separate explicit receive command uses a bounded TLS listener and mDNS
publication on awdl0. HTTP handles chunked messages and persistent connections.
Ask-only links and DVZip/CPIO uploads become inert files, with optional clipboard
output. No Apple ID authentication is implied by Everyone-mode receiving.
This is not the companion-link protocol and cannot replace its authentication.

Four simultaneous connections are allowed. Header lines, headers, trailers,
request bodies, decompressed archives, entry counts and filenames are bounded.
Only regular files are stored, using exclusive creation and flattened names.
Unit tests exercise a full TLS Ask/Upload exchange, including a traversal name,
without changing the desktop clipboard.

## Remaining hardware/protocol work

- Validate BCM4378 create/enable, channel schedule, peer table and outbound TCP.
- Validate the exact Pair-Verify nonce/framing and companion-link OPACK selectors
  against captures. Existing local tests establish internal consistency only.
- Implement/validate the separate large-pasteboard transfer channel. Inline
  pasteboard handling does not cover Apple's bulk channel.
- Validate `rpBA` correlation across Apple OS versions; explicit instance is the
  current fallback. `rpAD` authentication is not implemented.
- Validate Airdrop-compatible TLS/media capabilities and management-frame discoverability
  on a real Mac/iPhone; firmware service templates are not generated here.
- Implement a genuine Universal Clipboard server before presenting reverse
  clipboard synchronization as supported. The old panic-only broadcast stub
  and implicit wrapping nonce builder were removed.
- Measure power, throughput and latency before automatic radio-window activation
  or automatic channel changes. Band/schedule experiments are explicit today.

No runtime driver installation or network changes are performed by `ac-dc`.
