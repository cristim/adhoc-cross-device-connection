# handoff-clip

Receive Apple **Universal Clipboard** / **Handoff** announcements on Linux
(Asahi / any BlueZ box) by reusing the Continuity encryption keys that macOS
keeps in the iCloud keychain.

Apple's Continuity services (Handoff, Universal Clipboard, …) only talk to
devices signed into the *same Apple ID*, authenticated with keys that live in
the iCloud keychain. Linux can't join that keychain — but if you own a Mac (or
an Asahi machine that dual-boots macOS) that *is* signed in, the same keys are
already synced onto it. `handoff-clip` exports those keys once, under macOS,
and then uses them under Linux to observe and decrypt the Continuity traffic
your iPhone and Macs broadcast.

> **Status:** Milestone 1 (discover + decrypt the BLE advertisement) only. This
> tells you *the moment a nearby Apple device copies something* to the Universal
> Clipboard. Fetching the actual clipboard *content* is Milestone 2 (see
> Roadmap) and is not implemented yet.

> **Scope / ethics:** this only decrypts traffic from devices on *your own*
> Apple ID, using keys *you* export from *your own* Mac. It is interop /
> research for your own devices, in the spirit of the seemoo-lab work cited
> below.

---

## How it works

```
  macOS (native boot, same Apple ID as iPhone)
    └─ export-keys.sh ──> keys.json  (Continuity AES keys, iCloud-synced)
                              │  copy to Linux partition
                              ▼
  Linux / Asahi
    └─ handoff-clip scan --keys keys.json
         ├─ BlueZ: listen for Apple manufacturer-data BLE adverts (company 0x004c)
         ├─ parse the Handoff TLV (type 0x0c)
         ├─ AES-GCM decrypt with each key until the 1-byte tag authenticates
         └─ print "Universal Clipboard: a copy is available" when the flag is set
```

Because Asahi and macOS never run at the same time, we cannot read a *live*
keychain. The keys are long-term and iCloud-synced, so a one-off export is
enough; re-run it only if keys rotate or you add a device.

### Confirmed protocol facts

Reverse-engineered by seemoo-lab and documented in the USENIX 2021 paper
(citations below); re-derived here from their published Swift/Python code.

**Keychain item (macOS).** Generic-password items with service
`com.apple.continuity.encryption`. The item data is a binary plist containing
`keyData` (the AES key), `keyIdentifier`, `lastUsedCounter`. These are
`kSecAttrSynchronizable` — i.e. iCloud-synced to every same-Apple-ID device.

**BLE advertisement wire format** (Apple manufacturer data, company id
`0x004c`):

```
  4c 00 | 0c | LEN | STATUS | CTR(2, little-endian) | TAG(1) | CIPHERTEXT(10)
```

(BlueZ strips the `4c 00` company id from `ManufacturerData`, so on the wire we
see it start at the `0c` TLV — the parser accepts both framings.)

**Decryption: AES-GCM with non-standard parameters.**

| Parameter | Value |
|-----------|-------|
| Key       | `keyData` from the keychain (AES-128, 16 bytes) |
| IV        | the 2-byte advertisement counter (as-is, little-endian on the wire) |
| AAD       | the single plaintext `STATUS` byte |
| Tag       | **truncated to 1 byte** |
| Padding   | none |

The 2-byte IV and 1-byte tag are why we can't use the `aes-gcm` crate and
implement GCM from primitives (`aes` + `ghash`) in [`src/gcm.rs`](src/gcm.rs).

**Decrypted 10-byte payload:**

```
  STATUS(1) | ACTIVITY_HASH(7) | FLAGS(1) | UNUSED(1)
```

Flag bit `0x08` = **clipboard data available** (a copy just happened).
`ACTIVITY_HASH` is a truncated SHA-512 of the originating app's activity string.

---

## Validation ⚠️

**The crypto path is not yet validated against real traffic.** The unit test in
`src/gcm.rs` only proves the GHASH/CTR/J0 machinery is *internally consistent*
(encrypt-then-decrypt round-trips, wrong tag rejected). It does **not** prove we
reproduce Apple's exact framing — in particular:

- the derivation of `J0` for a sub-96-bit IV (we follow NIST SP 800-38D §7.1
  step 2b; CryptoSwift's `GCM` with a 2-byte IV must be confirmed to do the
  same),
- whether the counter bytes are fed to GCM in wire order or reversed,
- endianness / exact byte offsets of the TLV in your macOS/iOS version.

**To validate:** capture one real advertisement from your own iPhone (e.g. copy
text on the phone, capture the BLE manufacturer data via `btmon` or a sniffer),
then run:

```
handoff-clip decrypt --keys keys.json --data <manufacturer-data-hex>
```

If it decrypts to a sane 10-byte payload with the clipboard flag set, the path
is correct. If not, the offsets/IV-order in `src/gcm.rs` and `src/advert.rs`
are the first things to adjust. Until this passes on a captured packet, treat
M1 as "plausibly correct, unverified".

---

## Build & run

```
cargo build --release
cargo test               # runs the gcm + advert unit tests

# On Linux, with keys exported from macOS:
./target/release/handoff-clip scan --keys keys.json

# Offline decrypt of a single captured advert:
./target/release/handoff-clip decrypt --keys keys.json --data 0c0e08....
```

Requires a running `bluetoothd` (BlueZ) and a Bluetooth adapter. Set
`RUST_LOG=handoff_clip=debug` for verbose output.

### Exporting keys from macOS

See [`macos/export-keys.sh`](macos/export-keys.sh). Two paths:

- **Path A** — `security` CLI (quick; may be blocked by the item ACL on recent
  macOS).
- **Path B** — Frida-hook `rapportd` with seemoo-lab's `keychain_access.py`
  (reliable; needs SIP disabled once). Dumps *all* device keys.

Produce a `keys.json`:

```json
{ "keys": [ { "id": "<keyIdentifier>", "key": "<keyData-as-hex>" } ] }
```

(The loader also accepts a raw exported keychain binary plist directly.)

---

## Roadmap

- **M1 — discover + decrypt adverts** *(current)*. Detect on Linux when a nearby
  same-Apple-ID device copies to the Universal Clipboard. BLE only; no content.
- **M2 — pull clipboard content.** After a copy is announced, act as the
  companion-link client: discover the `_companion-link._tcp` service over
  mDNS, run Apple's **Pair-Verify** handshake (Curve25519 ECDH authenticated
  with the Ed25519 long-term keys from the `RPIdentity-SameAccountDevice`
  keychain identities), derive session keys, and pull the clipboard payload
  (**ChaCha20-Poly1305**, **OPACK**-encoded). Hand the result to `wl-copy`.
  - **Transport risk:** Apple normally runs this over **AWDL**. The Asahi Wi-Fi
    driver (`brcmfmac`) has **no monitor mode**, so the open-source AWDL stack
    (OWL) won't run here. **First test whether the exchange also works over the
    regular Wi-Fi / mDNS path** before committing to any AWDL work — if
    `_companion-link._tcp` is reachable over the LAN, M2 is tractable; if it's
    AWDL-only, it becomes a driver-level project.
- **M3 — reverse direction (Linux → Apple).** Emit our own Handoff adverts and
  serve companion-link requests so a copy on Linux pastes on the iPhone/Mac.

---

## References

- Stute et al., *Disrupting the Continuity of Apple's Wireless Ecosystem
  Security: New Tracking, DoS, and MitM Attacks on iOS and macOS Through Bluetooth
  Low Energy, AWDL, and Wi-Fi*, USENIX Security 2021.
  <https://www.usenix.org/conference/usenixsecurity21/presentation/stute>
- seemoo-lab / Open Wireless Link:
  - [`handoff-ble-viewer`](https://github.com/seemoo-lab/handoff-ble-viewer) —
    BLE advert parsing + decryption (basis for M1).
  - [`handoff-authentication-swift`](https://github.com/seemoo-lab/handoff-authentication-swift)
    — Pair-Verify + ChaCha20-Poly1305 + OPACK (basis for M2).
  - [`apple-continuity-tools`](https://github.com/seemoo-lab/apple-continuity-tools)
    — `keychain_access.py` for dumping keys (Path B export).
  - [`openwifipass`](https://github.com/seemoo-lab/openwifipass) — OPACK
    (de)serializer reference.

## License

MIT. Uses only your own devices' keys. Not affiliated with Apple.
