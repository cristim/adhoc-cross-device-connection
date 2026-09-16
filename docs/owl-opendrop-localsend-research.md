# OWL, OpenDrop and LocalSend comparison

Reviewed 2026-09-16. This supplements `omdrop-comparison.md`.

## What each project can contribute

| Project | Layer | Useful to ac-dc | Clipboard relevance |
| --- | --- | --- | --- |
| OWL | AWDL radio/link implementation in C | Availability-window scheduling, channel agreement, peer aging and transport diagnostics | No Universal Clipboard implementation found in the inspected source |
| OpenDrop | Airdrop-compatible application protocol in Python | Discovery/transfer connection lifetime, metadata and upload framing | No Handoff/Universal Clipboard implementation found |
| LocalSend | Independent LAN transfer protocol, Flutter UI and Rust core | Approval state machine, bounded streaming, cancellation, progress and an optional future LAN transport | Manual text/clipboard sharing; not Apple Universal Clipboard |

## OpenDrop: the most actionable sending difference

Inspected commit `11fe7ba7861093b302bc0637e8cb10adf2d29337`.

The CLI discovers recipients with one client, then constructs a new client for the transfer. `/Ask` and `/Upload` use that transfer client's connection. Our previous sender inserted `/Discover` immediately before `/Ask` on the transfer client. This is a plausible explanation for the observed successful discovery followed by a stalled Ask, but requires Apple-device validation.

The Rust change closes the discovery connection and begins the transfer with Ask, keeping Ask and Upload together. A loopback TLS regression test checks the two connections, endpoint order, a shared Ask/Upload transfer UUID, DVZip-wrapped CPIO, Apple upload headers, and chunked framing. A local test passing cannot establish that the Mac/iPhone accepts the transfer.

OpenDrop also uses scoped IPv6 endpoints. Its Darwin-only AWDL socket option is not a Linux fix. Its credential support is specific to Airdrop-compatible; it does not supply the identity/session machinery for Universal Clipboard. Our current Apple interoperability target remains Everyone mode.

Sources: [CLI](https://github.com/seemoo-lab/opendrop/blob/11fe7ba7861093b302bc0637e8cb10adf2d29337/opendrop/cli.py), [client](https://github.com/seemoo-lab/opendrop/blob/11fe7ba7861093b302bc0637e8cb10adf2d29337/opendrop/client.py), [README](https://github.com/seemoo-lab/opendrop).

## OWL: useful link diagnostics, not an HTTP upload replacement

Inspected commit `da255a70f221784c836d943dd3f243bc798f223b`.

OWL operates a userspace AWDL link using monitor mode and frame injection. It needs suitable hardware/driver support for active acknowledgments. Unicast transmission depends on known peers, matching availability windows and channel overlap, with a guard interval. Multicast has its own scheduled opportunities. Expired peers are removed.

Our BCM4378 firmware owns these scheduling decisions. Copying OWL's transmit loop into the Rust application would not control that firmware. The useful lesson is to distinguish interface readiness, received frames, TX completion/ACK evidence and successful application delivery. The helper now reports those observations separately. A TX completion is not proof that an upload reached a recipient.

If Ask still stalls after the connection fix, compare both devices' availability/channel advertisements and unicast ACK evidence before making more HTTP changes. Replacing the current firmware transport with OWL would be a separate hardware compatibility project, and risks the now-working receive path.

Sources: [OWL](https://github.com/seemoo-lab/owl), [scheduler](https://github.com/seemoo-lab/owl/blob/da255a70f221784c836d943dd3f243bc798f223b/src/schedule.c), [daemon](https://github.com/seemoo-lab/owl/blob/da255a70f221784c836d943dd3f243bc798f223b/daemon/core.c).

## LocalSend: transferable design and a possible future Rust backend

Reviewed official `main` sources on 2026-09-16; these links are moving references.

LocalSend uses a separate LAN protocol, not Apple's Airdrop-compatible endpoints or AWDL discovery. Normal use requires LocalSend on both devices; the protocol also describes reverse/browser transfers. Its approval response gives a session ID and per-file tokens. It supports rejecting a transfer, accepting selected files, cancellation and optional checksums. These fields cannot simply be added to Apple's protocol with the expectation that Apple will honor them.

Source: [protocol v2.2](https://github.com/localsend/protocol/blob/main/README.md).

The official repository contains a feature-gated Rust core using Tokio, Hyper, Reqwest and Rustls. This is worth evaluating if we later add a LocalSend-compatible LAN fallback, without rewriting ac-dc in Dart. No dependency or additional transport has been added by this review.

Source: [Rust core manifest](https://github.com/localsend/localsend/blob/main/packages/core/Cargo.toml).

Its server exposes explicit approval, upload, cancellation and session-end events, and binds upload authorization to the session, token and sender address. Drop guards clean up abandoned state. Its saving code streams through a bounded queue, reports progress and checks completion before replying successfully.

Useful equivalent changes in our pending Rust implementation are approval before consuming upload bodies, disk-backed streaming, cancellation guards, outgoing progress, and publishing a completed incoming archive atomically. LocalSend's session-token protocol is not copied into Airdrop-compatible. Future improvements include incoming byte progress and a bounded transfer history.

Sources: [server events and authorization](https://github.com/localsend/localsend/blob/main/packages/core/src/http/server/v2.rs), [streaming save](https://github.com/localsend/localsend/blob/main/packages/core/src/http/server/common/save.rs).

### Clipboard distinction

LocalSend's changelog documents clipboard sharing controls and desktop paste support. This is useful as a manual cross-platform text transfer experience. It is not Apple's automatic same-account Universal Clipboard. Automatic linked-device clipboard synchronization is still requested in issue #2971; an issue's status alone is not a complete implementation audit.

Sources: [changelog](https://github.com/localsend/localsend/blob/main/app/assets/CHANGELOG.md), [automatic clipboard sync request](https://github.com/localsend/localsend/issues/2971).

For Apple clipboard investigation, continue the separate Pair-Verify, Companion/Handoff selectors and pasteboard payload work in `clipboard-research.md`. None of these three projects supplies a verified implementation of that full path.

## Licensing and validation boundary

OWL and OpenDrop are GPL-3.0 projects. This review uses protocol observations and independently written Rust, without copying their implementation into the MIT application or GPL-2.0 driver. LocalSend's root license is Apache-2.0; check the exact component and dependency licenses before any future code reuse.

The user confirmed receiving with installed ac-dc 0.2.0-5 after the narrow AWDL firewall correction. The new connection, streaming and driver-helper changes require regression testing and a new real-device send test. Do not report outgoing Airdrop-compatible or Universal Clipboard as working on the strength of loopback tests.

### Installation follow-up

Version 0.2.1-1 is now installed for both app and driver. The application passes
79 local tests and the driver helper passes 16; clippy passes for both. The DKMS
module built and installed for the current kernel without unloading live Wi-Fi.
The restarted receiver handled TLS and Discover from both Apple devices. A real
outgoing file transfer remains unconfirmed; the user has been asked to test it.


### Upload timeout follow-up

The Mac now accepts Ask, but Upload on the same connection timed out even for
151 compressed bytes. All queued TCP bytes were acknowledged; the peer stopped
replying, including to the client's 15-second keepalive. The default Reqwest
30-second TCP_USER_TIMEOUT then reset the connection. The 0.2.1-2 experiment
closes each endpoint connection, so Upload gets a new TLS connection. This
intentionally departs from OpenDrop's reuse and is not yet hardware validated.
All 79 local tests still pass, including checking the separate connections.
The independent 1484-byte AWDL MTU correction did not resolve the whole timeout.

Fresh-connection experiment result: the 04:34 retry still timed out on Upload,
after Ask200 and acknowledgment of queued bytes on the new socket. Neither the
MTU correction nor connection separation resolves the full failure. Mac sharingd
logs are the next requested evidence; outgoing delivery remains unconfirmed.
