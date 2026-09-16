# Omdrop comparison and Airdrop-compatible failure investigation

Date: 2026-09-16. Scope: source comparison plus observations on this BCM4378 machine. This is not a claim of Apple interoperability certification or a completed transfer.

## Sources and method

Inspected the local snapshots of:

- [omdrop-plugin](https://github.com/brentkearney/omdrop-plugin/tree/daee7a3ed9620b5d1bad277f731fdf9eabc38d53), commit `daee7a3ed9620b5d1bad277f731fdf9eabc38d53`: README, `bin/omdrop`, `bin/airdrop-serve.py`, receiver service and panel behavior.
- [omdrop-awdl](https://github.com/brentkearney/omdrop-awdl/tree/0a7303da01757f7bdfa91be13b2b69b2c4d3b7fd), commit `0a7303da01757f7bdfa91be13b2b69b2c4d3b7fd`: patches, PKGBUILD, services, `omdrop-discoverable`, `awdl-up`, peer watcher, election/template generation, mDNS responder and Bluetooth advertiser.
- OpenDrop's local reference checkout `/tmp/opendrop-rust-reference`: client, server and TLS configuration, used to distinguish upstream protocol behavior from Omdrop-specific additions.
- Our current uncommitted Rust application and driver trees. Installed application at this review: `adhoc-cross-device-connection 0.2.0-5`; driver: `brcmfmac-awdl-local 0.2.0-3`.

These are pinned source snapshots; no assumption that upstream has stayed unchanged after these commits. Compared code paths, not just README feature lists. Local tests establish implementation consistency; only device observations establish interoperability.

## Findings that matter immediately

### 1. Confirmed installation gap: incoming Airdrop-compatible blocked by UFW

The receiver was listening on the AWDL link-local address, TCP 8771. A bounded header-only observation saw repeated incoming TCP SYNs to that port. The IPv6 firewall had default-drop input and user exceptions only for LocalSend port 53317, with no AWDL receiver exception. This is a concrete blocker to inbound discovery and receiving.

Applied the narrow rule:

```
ufw allow in on awdl0 proto tcp from fe80::/10 to any port 8771 comment 'ac-dc Airdrop-compatible receiver on AWDL'
```

Readback confirmed the rule and a counter of two accepted packets immediately afterward. It does not expose this port on infrastructure Wi-Fi or USB networking. This fixes a local installation omission; neither reviewed repository provides evidence of a complete firewall setup workflow. The rule alone does not prove a completed TLS handshake, a visible tile or file delivery.

The temporary observation prints TCP headers only, not file content. It originally recognized Ethernet-II IPv6 frames; driver-wrapped outgoing frames may not be represented. Therefore absence of outgoing lines alone must not be treated as proof that the kernel sent no reply.

### 2. Outgoing Ask failure remains unresolved

At 03:37:35 the Mac returned HTTP 200 to `/Discover`; our client then started a 309-byte `/Ask`. The user reported a transport timeout and no Mac approval prompt. The radio service remained running. This separates it from the earlier certificate rejection, but does not establish whether the Ask reached sharingd.

The reviewed Omdrop README explicitly says sending is not working yet. It is a receiver reference, not a working send implementation whose entire behavior can simply be ported. Our sender is based on OpenDrop's client protocol. The firewall correction explains inbound connection failures; it has not been shown to explain the outgoing Ask timeout.

### 3. Bluetooth diagnosis correction

Our v4 UI advertised the Bluetooth Airdrop-compatible wake message for only 12 seconds during recipient search. v5 keeps it alive during receiving too. However, deeper inspection shows Omdrop's continuous Bluetooth wake is **opt-in**, enabled by an `ble-wake` configuration file. Its comments explicitly warn that this advertises a sender and shares the radio with AWDL.

Thus continuous wake is an implementation difference and a testable option, not a proven missing receiver prerequisite. It should become an explicit setting or be scoped to outgoing discovery/transfer, and its effect should be measured before claiming it fixes incoming visibility. Our advertisement uses BlueZ broadcast type; their default is connectable/peripheral. That difference also needs controlled comparison, not an assumption that one is universally correct.

## Layer-by-layer comparison

| Layer | Omdrop behavior | Our Rust implementation | Assessment |
|---|---|---|---|
| Hardware | Documented BCM4387/M1 Pro validation | BCM4378, separately adapted driver | Chip-specific evidence must be repeated; upstream success is not proof for this firmware. |
| Kernel interface | Create AWDL netdev, specialized operations, safe teardown | Equivalent concepts, with additional host carrier/queue fix | Core functionality present; installed module has been verified loaded. |
| Data path | AWDL SNAP encapsulation/decapsulation, stale flow-ring TX-status guard | Adapted patches 7/8 and host-queue patch 9 | Present. Device discovery over TCP/TLS has succeeded. |
| Event delivery | Action-frame signature scanning in management forwarding | Raw firmware vendor events, Rust signature scan and decoder | Different design, same needed observations available. Missing diagnostic patch numbers are not automatically missing functionality. |
| Firmware RAM access | Diagnostic RAM snapshot vendor operation | Not included | Not required for normal transfer; do not add merely for parity. |
| Scheduling | Shared configurable peer/master channels and channel shapes | Discoverable hardcodes dense(44,6); standalone window supports explicit schedules | Confirmed integration gap. A generic window option does not mean the UI uses it. |
| Wi-Fi coexistence | Plugin moves infrastructure connection to 2.4 GHz and restores it | BandLease exists for explicit driver window, not UI discoverable path | Confirmed missing integration, relevant when infrastructure is on 5 GHz. Do not blindly switch a working connection without measuring current band. |
| Election | Metric 100, optional RSSI threshold writes, settling delay, repeated stable observation | Metric 100, firmware state mirrored into TLV24, two observations | Basic mechanism present; missing optional threshold/settling controls and one hysteresis reset bug. |
| Peer registration | Register on first raw data frame; permanent IPv6 neighbour; bounded peer set | Register from management or data source MAC; neighbour; bounded set | Present. Our post-ADD readback failure can make a successful registration appear failed. |
| PSF template | Length-prefixed payload, firmware-owned TLVs omitted, HT/VHT/datapath fields | Same ABI strategy and corresponding fields | No obvious missing essential TLV from source comparison. Byte layout correctness still requires device validation. |
| Service template | Bare Arpa/PTR/SRV/TXT TLVs in `awdl_afs_pload` | Same layout; flags 137; MAC-derived instance and host | Present and consistent with app mDNS identity. |
| mDNS announcements | Periodic bursts, same service identity everywhere | Periodic 8-frame burst plus unicast peer announcements | Present. |
| mDNS query replies | Raw responder: 5-frame replies, 100 ms rate limit, explicit NSEC for missing IPv4 A | App uses mdns-sd; radio raw socket only extracts source MAC, no query-triggered reply | Important reliability gap. Library answers normal queries; it is not equivalent to the upstream AWDL-specific burst responder. NSEC parsing support alone does not establish equivalent emission. |
| Neighbour nudging | Regular unicast traffic makes peer learn our address | Periodic unicast announcement to registered peers | Equivalent intent already implemented. |
| Readiness | Transmit probe, receive observation, distinguishes transmit-only from receive-proven | Link/IP readiness; nullable traffic proof in status; discovery.json is just active | Major UI/status gap. Active is not proof that Apple can connect. |
| Receive lifetime | One supervised deadline, later start extends it; teardown follows deadline | Separate 660-second root service and 600-second receiver; start of active service does not extend root lifetime | Confirmed bug risk: receiving can outlive radio. UI `radio_owned` also becomes stale after expiry. |
| Recovery | Differentiates parked/template-loaded radio; avoids unsafe disable; explicit reload path | Refuses unsafe teardown with loaded template, leaves firmware enabled and lowers host link | Safety concept present; recovery diagnostics and UI guidance are less complete. |
| TLS identity | Self-signed RSA-2048, OpenSSL TLS stack, no verified Apple ID identity | Self-signed RSA-2048, Rustls, same user-owned keypair for client/server | No evidence that certificate regeneration is a fix. Negotiation differences remain a compatibility test item. |
| Discover response | Name, model, JSON bytes for media capabilities in binary plist | Same three fields; JSON `{\"Version\":1}` bytes | Present. Media capabilities are JSON bytes, not an embedded plist. |
| Ask/Upload connection | OpenDrop holds one HTTPS connection | reqwest pool normally reuses connection; no explicit single-stream guarantee | Potential compatibility difference, not proven cause. Current local wire test exercises same-connection flow. |
| Sender identity | OpenDrop SenderID is configured service ID | Random per-transfer SenderID; advertised instance is MAC-derived | Confirmed discrepancy; align the identity rather than inventing new values each send. Apple rejection from this is unproven. |
| Ask file metadata | FileName, FileType UTI, BomPath, directory flag, media conversion flag; optional icon | Same basic file fields, smaller UTI mapping, no icon, model `ac-dc` | Optional icon/header differences do not justify declaring root cause. Folders remain unsupported here. |
| HTTP framing | Persistent reader, chunked upload dechunking, gzip CPIO from sender | Bounded parser, chunked receive and send, gzip CPIO, 100-continue | Main capabilities present. v4 fixed our fixed-length Upload difference. |
| Incoming acceptance | Reference receiver auto-accepts while enabled | UI requires Accept/Decline; explicit CLI receive mode auto-accepts | Intentional behavior difference; keep UI approval. |
| Received data | DVZip stored/zlib blocks, CPIO, skip AppleDouble, inert URL files | Corresponding support with bounds, flat files and collision avoidance | Present. Need actual iPhone/Mac tests for each representation. |
| Large transfers/folders | Broader OpenDrop/libarchive behavior | 64 MiB encoded/decoded bounds, entire archive in memory, regular-file sending only | User-requested full functionality not yet met. Streaming and directory semantics are real remaining work. |
| File publication | Exclusive file creation and name cleaning | Exclusive 0600 final-path writes; cleanup on ordinary error | Crash can leave partial final-name files; atomic staging is a worthwhile improvement, not established Omdrop parity. |
| Desktop UX | Bar integration, countdown/options, once/always modes, notification actions, name/directory settings | Native GTK chooser, recipients, links, approval UI, 10-minute fixed window | Rust UI exists; progress, cancellation, transfer history and reliable session state remain incomplete. |
| Privilege boundary | Root-owned narrow helper and named polkit action | Root-owned awdlctl and system service through pkexec | Separation present. Password-free seat authorization is not needed for protocol correctness. |
| Service confinement | Receiver service uses filesystem and syscall restrictions | UI-hosted receiver has normal user access; optional service simpler | Useful hardening gap; adapt restrictions to GTK/Wayland rather than copying blindly. |
| Packaging | DKMS with stock module fallback, vendored narrow kernel sources | Kernel-specific installed module and local absolute-path recipe | Confirmed maintenance gap: next kernel needs a rebuild. |
| Diagnostics | Staged radio state, query counts, receiver request logs | Sender stage logs; receiver connection errors mostly debug; no TCP/TLS/request counters in UI | Makes current failures hard to isolate; add stage/error telemetry without file contents. |

## Specific code corrections justified by review

1. **Firewall install/check documentation:** include the narrow IPv6 receiver rule, its removal command, and a health warning when a listener exists but blocked inbound traffic is observed. Do not silently disable UFW or open all AWDL ports.
2. **Election hysteresis:** clear pending state when current firmware state returns to the already-published state. Current Rust code keeps an old pending value, so observations B,A,B can publish B without consecutive B samples. Omdrop clears it.
3. **Deadline/state ownership:** one expiry for receiver and root radio, support extension, and re-check actual service readiness instead of relying on `radio_owned`.
4. **mDNS responder:** retain a single identity across raw and ordinary announcements; implement bounded query-triggered bursts and explicit negative A answers where needed. First distinguish firewall/socket delivery from over-the-air loss.
5. **Stable SenderID:** derive it from the same service identity used for discovery, while preserving portable loopback tests.
6. **Receiver diagnostics:** log peer TCP acceptance, successful TLS, endpoint/status and elapsed time. Never dump private keys, clipboard contents, request plists, URLs or file data to ordinary logs.
7. **Self filtering:** remove the local address from recipient results. The current browser can discover its own receiver and inflate the reported device count.

These are ordered work items, not claims they are all implemented by this report. At report creation only the firewall correction is newly applied during this comparison; v5 Bluetooth behavior was already installed before it.

## Validation order

1. Verify the UFW rule receives packets and an Apple TCP handshake completes to 8771 while our listener is live. Then verify inbound TLS and `/Discover`, followed by a visible Omarchy target on each Apple device.
2. One small regular file from Mac to Linux, then iPhone to Linux. Verify saved bytes and approval behavior, not just HTTP status.
3. For outgoing attempts, observe whether Ask bytes are acknowledged. If unacknowledged, investigate driver scheduling/peer state rather than plist fields. If acknowledged without a prompt, compare the application exchange, stable SenderID and connection lifetime; Mac sharingd logs may be necessary.
4. Validate user accept/decline, links, images, mixed files, collision handling, cancellation and expiration. Separate fresh Everyone-mode tests from Contacts Only, which is not implemented.
5. Only after small transfers work, add streaming/large files and folders. Keep clipboard protocol testing independent: Airdrop-compatible success does not establish Universal Clipboard compatibility.

## Current conclusion

We have most basic receiver wire formats and essential driver mechanisms, but not Omdrop's full operational integration. The strongest finding is the local firewall omission; missing traffic-based readiness and receiver diagnostics hid it. Other confirmed implementation gaps explain why a short successful discovery did not justify presenting complete send/receive support. No end-to-end file transfer has yet been confirmed in this investigation.


## Follow-up implementation status — 2026-09-16

The table above is the initial audit snapshot. The user subsequently confirmed
receiving on 0.2.0-5 after the narrow firewall rule was installed. Its final
sentence saying no file transfer was confirmed is superseded by this update.

Changes prepared for 0.2.1:

- Renewable root deadline and receiver monitoring of actual radio state.
- Consecutive-sample election hysteresis and bounded mDNS query responses/NSEC.
- Stable SenderID, self filtering and TCP/TLS/request diagnostics.
- Disk-backed uploads/downloads, folder archives, cancellation of blocking work,
  approval before reading Upload bodies, and atomic publication of received
  archive directories and individual link files.
- UI outgoing progress, cancellation, folder selection and radio countdown.
  Bluetooth wake remains enabled by default and is now optional.
- Narrow firewall installation documentation; no automatic broad port opening.
- Basic hardening for the optional CLI receiver service. The GTK UI remains a
  normal user process because it also starts the privileged radio helper.
- Vendored narrow kernel sources and DKMS packaging, preserving the stock module.
- OpenDrop connection sequencing correction; see the new three-project review.

Validation includes local TLS, approval/decline, chunked gzip CPIO, a 65 MiB file,
folder round-trip, hostile paths, publication and cancellation tests. Apple-device
sending still needs validation. Incoming byte-progress UI, persistent transfer
history and automatic firewall diagnosis are not implemented. Contacts Only,
empty-folder-only transfers and Universal Clipboard are also remaining limits.
# 0.2.1 validation — 2026-09-16

- Installed adhoc-cross-device-connection 0.2.1-1 and brcmfmac-awdl-local 0.2.1-1.
- App: 79 tests pass; driver helper: 16 tests pass; both clippy -D warnings pass.
- Full external kernel module compilation passed against 7.1.13-2-1-ARCH.
- DKMS reports brcmfmac-awdl/0.2.1 installed, original modules retained.
- modinfo selects updates/dkms/brcmfmac.ko. No live Wi-Fi module unload occurred.
- Package hook regenerated initramfs successfully; existing aarch64 microcode,
  drm_privacy_screen_register and dockchannel_hid warnings were printed.
- New UI runs as ac-dc-ui.service, PID 1264746.
- New receiver accepted TLS and /Discover from both Mac and iPhone at 04:17.
- At 04:18 radio state showed received_frames=92, tx_completions=194,
  tx_acked=88, rx_proven=true, tx_proven=true. These are not file delivery proof.
- Outgoing real-device transfer still awaits the user's test. The pending async
  question asks whether the new UI produces a prompt and completes a small file.
- The user previously confirmed receiving on 0.2.0-5 after the narrow UFW rule.

Source repos contain changes, no new commits or pushes. The final PKGBUILD work
path correction was tested with makepkg --nobuild after package installation;
it changes packaging scratch location only, not installed module source.
Generated accidental src/kernel and old absolute-path packaging/local-install
were moved to /tmp/acdc-fixes/accidental-package-workdir and legacy-local-install.

Still open: incoming byte progress, persistent history, automatic firewall
health diagnosis, empty-folder-only archives, real Apple send validation,
Contacts Only and full Universal Clipboard protocol/authentication.
