# Universal Clipboard research and implementation audit

Research date: 2026-09-16. Local application base: `5366484907ff91bd31faa6442865ba62e141df20`; Airdrop-compatible work is occurring concurrently. This report describes the clipboard files as inspected, not a new interoperability test.

## Executive conclusion

The application has useful BLE scanning, authentication primitives, transport orchestration and Wayland output. **Its current companion-link implementation cannot yet be treated as an interoperable Universal Clipboard client.** Several supposed unknowns are demonstrable disagreements with the original research implementation: encrypted frame lengths, authenticated headers, OPACK encoding, request names, message types and response structure. Fix those before interpreting connection failures as Wi-Fi-driver failures.

Airdrop-compatible and Universal Clipboard can share AWDL networking and a UI, but they require separate discovery, authentication and application protocols. An Airdrop-compatible transfer does not validate clipboard authentication. Receiving a file into the clipboard is a useful Airdrop-compatible feature, not native Universal Clipboard.

The highest-value experiment is a successful **iPhone → Mac and Mac → iPhone native clipboard baseline**, followed by a short, instrumented Linux pull from one identified peer. Current Apple-version trust acceptance remains a research question. Do not weaken signature verification to get past it.

### Evidence vocabulary

- **Confirmed in source:** directly visible in the inspected code or cited primary source; not necessarily observed on the user's hardware.
- **Inference:** a consequence or proposed implementation approach that needs testing.
- **Unknown on current devices:** requires the user's actual OS versions, captures or controlled interaction.

No private key files, encrypted macOS volumes or user applications were accessed. No radio state was changed. Research did not install tooling on the Mac or iPhone.

## 1. Sources and reproducibility

Primary reference checkouts reviewed:

| Reference | Revision | Purpose |
| --- | --- | --- |
| [handoff-authentication-swift](https://github.com/seemoo-lab/handoff-authentication-swift/tree/4518b75b6bc5c66decc40fefba49d0bb7adab21d) | `4518b75b6bc5c66decc40fefba49d0bb7adab21d` | Pair-Verify, encrypted transport, UC messages, archived pasteboard, keychain prototype |
| [handoff-ble-viewer](https://github.com/seemoo-lab/handoff-ble-viewer/tree/733ba6dae2114c1cbdd70d2399c7025ec3759dbb) | `733ba6dae2114c1cbdd70d2399c7025ec3759dbb` | BLE layout, GCM inputs, status flags |
| [apple-continuity-tools](https://github.com/seemoo-lab/apple-continuity-tools/tree/b93207b5887c6bef8c9e0e16a442149fb3520e28) | `b93207b5887c6bef8c9e0e16a442149fb3520e28` | Rapport observation points, keychain research |
| [pyatv OPACK implementation](https://github.com/postlund/pyatv/blob/master/pyatv/support/opack.py) | `master`, read on research date | Independent, actively used Companion serialization implementation; not a UC implementation |
| [OpenWifiPass OPACK notes](https://github.com/seemoo-lab/openwifipass/blob/main/OPACK.md) | `main`, read on research date | Provenance and limitations of the older encoder |

The three pinned checkouts are available under `/tmp/clipboard-handoff-auth`, `/tmp/clipboard-handoff-ble`, and `/tmp/clipboard-continuity-tools`. They are research inputs, not application dependencies. License obligations must be checked before copying implementation code; the Rust implementation should document independently implemented protocol facts and fixture provenance.

Two crucial scope limits: the [Swift reference README](https://github.com/seemoo-lab/handoff-authentication-swift/blob/4518b75b6bc5c66decc40fefba49d0bb7adab21d/Handoff-Swift/README.md) describes a partial UC implementation and reports failure at the final authentication step on macOS 11/iOS 14. Its hard-coded identities and advertisement examples are not usable account configuration. Research-era compatibility is not evidence of current compatibility.

Apple's [Universal Clipboard setup guide](https://support.apple.com/en-us/102430) is the prerequisite checklist: nearby supported devices, the same Apple Account, Bluetooth, Wi-Fi and Handoff enabled. Apple's [Handoff security description](https://support.apple.com/en-ie/guide/security/secf78dbe639/web) explains account-established trust, encrypted advertisements and a separate protected path for larger payloads. Neither provides a supported Linux onboarding API.

## 2. Protocol boundary and order

For this application the investigation should follow:

1. Identify a fresh BLE Handoff advertisement and its originating device.
2. Discover that peer's companion-link endpoint on a verified interface.
3. Complete mutually authenticated Pair-Verify.
4. Exchange authenticated OPACK messages with correct framing and transaction IDs.
5. Request pasteboard metadata and content, decoding the archive safely.
6. Negotiate a bulk channel when requested by the peer.
7. Offer one suitable representation to Wayland without creating a synchronization loop.

AWDL readiness alone establishes none of steps 2–6. Conversely, a source-level serialization defect remains a defect even when the radio works perfectly. Keep diagnostics per stage: `advertisement_seen`, `candidate_resolved`, `tcp_connected`, `peer_verified`, `local_identity_accepted`, `metadata_received`, `content_received`, `clipboard_owned`.

## 3. BLE: mostly recognizable layout, insufficient identity and lifecycle handling

The local `advert.rs` layout agrees with the [reference parser](https://github.com/seemoo-lab/handoff-ble-viewer/blob/733ba6dae2114c1cbdd70d2399c7025ec3759dbb/BLE-Viewer/HandoffModels/HandoffBLE.swift): Continuity type `0x0c`, status, two-byte IV, one-byte authentication tag and encrypted body. The [reference decryptor](https://github.com/seemoo-lab/handoff-ble-viewer/blob/733ba6dae2114c1cbdd70d2399c7025ec3759dbb/BLE-Viewer/Crypto/BLEDecryptor.swift) supplies the status as GCM additional authenticated data. The [decrypted model](https://github.com/seemoo-lab/handoff-ble-viewer/blob/733ba6dae2114c1cbdd70d2399c7025ec3759dbb/BLE-Viewer/HandoffModels/HandoffAdvertisement.swift) contains the activity hash, availability bit `0x08` and pasteboard-version bit `0x10`.

Local issues confirmed by inspection:

- `HandoffBle::parse` only accepts Handoff as the first TLV. It should walk a bounded TLV sequence, reject truncation, and tolerate other Continuity TLVs ahead of Handoff.
- A one-byte GCM tag is a weak key selector. For unrelated keys, a random tag match has roughly 1/256 probability; with multiple keys, the first apparent match can misidentify a device. `HandoffPayload::parse` only checks minimum length. Preserve all plausible candidates, validate expected layout and observations across packets, and require a known signing identity before accepting content. Never equate BLE tag success with authenticated device identity.
- `CopyEvent` throws away the original advertisement, flags, IV, receive timestamp and version bit. Later request construction may need those facts; an activity label is not a substitute.
- Deduplication by key and `(counter, activity_hash)` happens before enqueueing. A full queue or transient fetch error can consume the only retry opportunity for that copy. Track observations separately from completed fetches; expire stale work and retry a fresh generation within a bounded budget.
- Device watcher tasks are detached and may accumulate across repeated discovery events. Tie watchers to device/session lifetimes.
- BLE keys are loaded once and there is no key renewal exchange. Diagnose stale exports distinctly from malformed packets. Do not invent a rotating advertising counter using an old encryption key.

The BLE activity hash identifies an activity, not the copied text. UI wording should say “clipboard available” until content is actually retrieved.

## 4. Discovery: `rpBA` is not a proven BLE-address equality key

The 2021 [Continuity paper, §4.1](https://www.usenix.org/system/files/sec21fall-stute.pdf) reports that companion-link advertises `rpBA` and `rpAD`; the former is randomized and the latter derives from it and the device IRK through SipHash. It describes account-key-based filtering, not a guarantee that `rpBA` equals the BLE advertiser address. It also describes BLE-key renewal before the two-byte IV space is exhausted. For large clipboard payloads, it reports a TLS path above 10,240 bytes, bootstrapped by the payload exchange, with Apple identity validation. These are historical measurements, not a current-version specification.

`discover.rs::resolve` currently requires string equality between `rpBA` and `CopyEvent.address` unless an explicit instance is selected. That can discard the correct peer before authentication. The identity JSON stores peer public keys but **no per-peer IRK**, so implementing authenticated TXT correlation also needs a schema extension. Do not guess SipHash input packing, truncation or key conversion; capture a known pair and add a vector.

Use an explicitly selected Mac instance for the first experiment. Treat names, addresses and TXT records as hints; the signing key is the authority. Return the authenticated peer label/fingerprint from Pair-Verify so orchestration can check that it fetched the intended device.

A useful later experiment is explicit infrastructure-LAN companion-link discovery. The [Rapport research transcript](https://github.com/seemoo-lab/apple-continuity-tools/blob/b93207b5887c6bef8c9e0e16a442149fb3520e28/continuity_messages/README.md) includes an Ethernet link type. This establishes that Companion is not intrinsically AWDL-only, but does not prove current UC requests work over every LAN endpoint. The present unconditional radio check blocks that investigation.

## 5. Authentication and identity: keep three key purposes separate

| Material | Purpose | Current gap |
| --- | --- | --- |
| Continuity AES advertisement key | Understand nearby activity announcements | Export and scan exist; renewal not implemented |
| Local Ed25519 seed plus trusted peer public keys | Pair-Verify signatures | Acceptance of our identity by current devices unverified |
| Per-peer device IRK | Same-account discovery correlation | Missing from peer schema |
| Apple identity/certificate material for bulk negotiation | Authenticate negotiated bulk transport | Not implemented; not interchangeable with an Airdrop-compatible Everyone self-signed certificate |

The [reference verifier](https://github.com/seemoo-lab/handoff-authentication-swift/blob/4518b75b6bc5c66decc40fefba49d0bb7adab21d/Handoff-Swift/Sources/Communication/Pairing/PairingVerifier.swift) reads same-account public identities and IRKs. The [keychain insertion prototype](https://github.com/seemoo-lab/handoff-authentication-swift/blob/4518b75b6bc5c66decc40fefba49d0bb7adab21d/Handoff-Swift/Sources/Crypto/MacKeychainController.swift) demonstrates an attempted synchronizable item insertion. A successful write does **not** prove Rapport consumes it, iCloud sync distributes it as intended, or the iPhone trusts it.

Correction needed in `macos/RPIDENTITY.md`: its claim that the Ed25519 secret is probably a non-exportable Secure Enclave key is unsupported. Apple's [documented Secure Enclave signing API](https://developer.apple.com/documentation/cryptokit/secureenclave/p256) uses P-256. That does not establish where Rapport stores its Ed25519 secret, but it makes the current confident inference inappropriate. A failed `SecKeyCopyExternalRepresentation` call alone does not identify the cause; key type, attributes and error context matter. The correct status is **unknown storage/exportability**, not “very likely impossible.”

Retain rejection of unknown signatures and low-order X25519 points. Add explicit expected message type/state/error validation and preserve the verified peer identity. The Pair-Verify reference uses the 64-bit-nonce primitive with `PV-Msg02`/`PV-Msg03`; verify equivalence to the Rust zero-prefixed RFC nonce with an independent vector before changing it. Content nonces are a separate convention: the reference appends zeros to the little-endian counter, as the Rust implementation does. Do not casually make these two nonce constructors identical.

## 6. Confirmed encrypted transport defects

The [reference packet](https://github.com/seemoo-lab/handoff-authentication-swift/blob/4518b75b6bc5c66decc40fefba49d0bb7adab21d/Handoff-Swift/Sources/Communication/ContinuityPacket.swift) constructs a header from plaintext size plus the tag. The [encryption handler](https://github.com/seemoo-lab/handoff-authentication-swift/blob/4518b75b6bc5c66decc40fefba49d0bb7adab21d/Handoff-Swift/Sources/Communication/HandoffHandler.swift) encrypts with that header as AAD, then appends the tag and retains the header. Its [cryptor](https://github.com/seemoo-lab/handoff-authentication-swift/blob/4518b75b6bc5c66decc40fefba49d0bb7adab21d/Handoff-Swift/Sources/Crypto/HandoffCryptor.swift) authenticates the received header too.

Local `send_opack` already has ciphertext **including its tag**, but `serialize()` adds another 16 to the advertised length. The reader subtracts 16 to agree with its own writer. A real peer instead reads the advertised encrypted body. That creates truncation or waiting for bytes that will never arrive. `CONTENT_AAD` is empty, another direct disagreement.

Required invariant: for encrypted frames, header length equals actual wire ciphertext-plus-tag length. Construct the header before encryption, authenticate it, send it unchanged, and read exactly the advertised body. Support or explicitly reject the full three-byte length field; the reference decoder uses all three length bytes while our reader ignores header byte 1. Enforce checked size limits before serialization rather than narrowing to `u16`.

The existing roundtrip test deliberately locks in the wrong shared assumption. Replace it with independent golden frames, partial/coalesced stream reads, header-tampering rejection and boundary-length tests. A mock built with the same writer and reader cannot establish interoperability.

## 7. OPACK and actual clipboard messages

### OPACK

The [independent pyatv implementation](https://github.com/postlund/pyatv/blob/master/pyatv/support/opack.py) uses little-endian scalar/length encodings and integer widths 1, 2, 4 and 8. The Rust code uses big endian and consecutive widths 1, 2, 3 and 4. Byte-string length markers also have different widths. Thus even a transaction ID or longer archive can decode incorrectly. Object references, null, UUID and numeric forms needed by real responses are absent. Add resource/depth limits and full-input consumption; reject unsupported values explicitly. Cross-check fixtures from more than one implementation, since the older OpenWifiPass encoder itself is labeled research-only.

### Request and response envelope

The [reference UC controller](https://github.com/seemoo-lab/handoff-authentication-swift/blob/4518b75b6bc5c66decc40fefba49d0bb7adab21d/Handoff-Swift/Sources/Communication/UniversalClipboardController.swift) gives concrete historical wire names:

| Field | Local implementation | Reference |
| --- | --- | --- |
| Request/response `_t` | 1 / 2 | 2 / 3 |
| System-info selector | `SystemInfo` | `_systemInfo` |
| System-info content | Top-level name/model/os | Dictionary in `_c`, plus `_x` |
| Clipboard selector | `FetchPasteboard` | `com.apple.handoff.payload-request` |
| Clipboard operation | No command/content dictionary | `_c` includes `rClientCommand`, `rIdentifier`, `rAdvPayload` |
| Commands | None | `pbtypes`, `pbpaste2` |
| Response content | Invented top-level `items` list | `_c.rActPayload` containing a keyed archive |

Do not copy the reference's hard-coded `rAdvPayload`, account identifier or device metadata. Their semantics and current requirements need observation. The controller itself contains unfinished server handling and apparent copy/paste mistakes; use it as evidence, not a production implementation.

Implement a bounded response dispatcher: validate `_x` and response kind, surface remote errors, and handle unrelated events without mistaking them for the outstanding response.

### Pasteboard archive

The [reference model](https://github.com/seemoo-lab/handoff-authentication-swift/blob/4518b75b6bc5c66decc40fefba49d0bb7adab21d/Handoff-Swift/Sources/Model/SharedPasteboard/UASharedPasteboardInfo.swift) describes `UASharedPasteboardInfoWrapper`, item/type metadata, extra data, offsets, sizes and protocol version. Rust should parse the binary plist/keyed-archive graph with an allowlist and strict bounds, not instantiate arbitrary classes. Preserve multiple representations per item. Verify offset-plus-size arithmetic and use end-exclusive slices; do not reproduce the Swift example's potentially inclusive endpoint.

Small text is the first target. Bulk connection setup, TLS identity validation, representation selection and images/files are distinct milestones. Airdrop-compatible archive parsing is not a substitute for the UC keyed archive.

## 8. Reverse clipboard synchronization

The current mock server is not a production Universal Clipboard server. Linux → Apple needs lifecycle-managed local clipboard observation, generation tracking, trustworthy Handoff advertisements, companion-link publication, an authenticated server, metadata/content responses and bulk handling. It also needs echo suppression and a user-visible enable/disable control. Copying a received item must not announce it again as a new local copy.

Design the UI around independently verified capabilities: “Receive from Apple,” “Share Linux clipboard,” “Airdrop-compatible send,” and “Airdrop-compatible receive.” Do not show native clipboard synchronization as ready because Airdrop-compatible discovery works.

## 9. Hardware investigation plan for this user's iPhone and Mac

### A. Establish the baseline

Record OS/build, Mac model, phone model, Handoff settings and whether both devices use the same Apple Account; do not record account credentials. Test native copy/paste in both directions using an innocuous unique token. Then repeat with a small PNG and a larger synthetic text payload. Log timestamps, direction, size and outcome. If native exchange fails, resolve that before Linux testing.

### B. Observe without modifying trust

On the Mac, browse `_companion-link._tcp` with `dns-sd`; record interface, SRV endpoint and TXT properties for a bounded interval. Use a narrowly scoped `log stream` predicate for `rapportd` and `useractivityd`. Capture only the deliberate synthetic transfer on the relevant interface when packet capture is needed. Keep captures private and out of Git; encrypted traffic still contains identifying metadata. On Linux, compare BLE raw frames and event timing without printing keys.

Compare copy, paste, repeated same-text copy, clearing clipboard, locking and unlocking. Determine whether the Mac prefetches metadata on announcement and only fetches content on paste. Do not infer the trigger from a single advertisement.

### C. Repair offline protocol fixtures first

Implement the frame/AAD fixes, OPACK vectors and real UC envelopes before hardware retries. Include unknown-peer, malformed-state, bad-tag, invalid-length and archive-offset rejection. Keep external fixtures synthetic or sanitized and record their origin. Re-run known-good loopback tests only after they model the independent wire contract.

### D. Separate trust from payload failures

Use one explicit Mac endpoint. Log M1 sent, M2 validated, M3 sent and M4 outcome separately, followed by system-info and clipboard result. Do not dump signing keys, session secrets or decrypted personal clipboard content. If M4 fails, investigate trust acceptance and remote errors; do not change driver scheduling or disable authentication as a workaround.

Only after successful peer authentication investigate selector/version differences. If deeper observation is necessary, [Continuity message tools](https://github.com/seemoo-lab/apple-continuity-tools/tree/b93207b5887c6bef8c9e0e16a442149fb3520e28/continuity_messages) identify Rapport send/receive object boundaries. Their old LLDB scripts are not a ready-made modern Mac procedure: symbols, architecture, entitlements and debugging restrictions may differ. Attaching to system daemons or changing protection settings should be a separate informed experiment, not an automatic setup step.

### E. Acceptance matrix

| Direction/content | Required evidence |
| --- | --- |
| Mac → Linux short text | Fresh generation, known peer signature, matching content bytes, usable Wayland clipboard |
| iPhone → Linux short text | Same, independently from Mac result |
| Either Apple device → Linux image | Correct representation and decoded image; bulk path when negotiated |
| Linux → Mac / iPhone | Apple initiates authenticated request after Linux copy; paste succeeds on each device |
| Multiple peers | Triggering device remains bound to verified signer; no cross-device clipboard mix-up |
| Reconnect/rotation | Bounded retries, no stale paste, no nonce reuse, renewed-key failures diagnosed |
| Disabled or expired session | No automatic fetch/advertisement continues; no secret payload left in logs |

## 10. Implementation priorities after Airdrop-compatible

1. **Wire correctness:** fix encrypted lengths/AAD and OPACK with independent fixtures.
2. **Honest diagnostics:** exact authentication stage, remote errors and authenticated peer identity.
3. **Real UC request/response:** system-info, metadata, small text and bounded keyed-archive parsing.
4. **Discovery/credentials:** per-peer IRK support, validated TXT correlation, explicit peer selection, current-device trust experiment.
5. **Reliability:** fresh-event state, retries, task lifetime, clipboard ownership and echo prevention.
6. **Bulk and reverse direction:** validate negotiated TLS and implement actual server behavior.
7. **Automatic integration:** only after measured iPhone and Mac success; retain separate Airdrop-compatible/clipboard status in the UI.

The immediate next clipboard result worth claiming is **one authenticated short-text pull from the user's Mac**, followed independently by the iPhone. The source review establishes a concrete repair roadmap; it does not establish either transfer has occurred.
