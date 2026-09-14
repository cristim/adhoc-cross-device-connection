# ac-dc

Receive Apple **Universal Clipboard** / **Handoff** announcements on Linux
(Asahi / any BlueZ box) by reusing the Continuity encryption keys that macOS
keeps in the iCloud keychain.

Apple's Continuity services (Handoff, Universal Clipboard, …) only talk to
devices signed into the *same Apple ID*, authenticated with keys that live in
the iCloud keychain. Linux can't join that keychain — but if you own a Mac (or
an Asahi machine that dual-boots macOS) that *is* signed in, the same keys are
already synced onto it. `ac-dc` exports those keys once, under macOS,
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
    └─ ac-dc scan --keys keys.json
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
ac-dc decrypt --keys keys.json --data <manufacturer-data-hex>
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
./target/release/ac-dc scan --keys keys.json

# Offline decrypt of a single captured advert:
./target/release/ac-dc decrypt --keys keys.json --data 0c0e08....

# No keys needed:
./target/release/ac-dc capture     # grab a raw advert to validate the decrypt
./target/release/ac-dc discover     # browse _companion-link._tcp (the M2 AWDL test)
```

Requires a running `bluetoothd` (BlueZ) and a Bluetooth adapter. Set
`RUST_LOG=ac_dc=debug` for verbose output.

### Exporting keys from macOS

Easiest: build the Swift helper (see
[`macos/ac-dc-send/README.md`](macos/ac-dc-send/README.md)) and run `ac-dc
export-keys`. It queries the keychain via `SecItemCopyMatching`, so it gets
every key with SIP left enabled, and writes `keys.json` at mode 0600.

Otherwise [`macos/export-keys.sh`](macos/export-keys.sh) offers two paths:

- **Path A** — `security` CLI (quick; may be blocked by the item ACL on recent
  macOS).
- **Path B** — Frida-hook `rapportd` with seemoo-lab's `keychain_access.py`
  (reliable; needs SIP disabled once). Dumps *all* device keys.

Produce a `keys.json`:

```json
{ "keys": [ { "id": "<keyIdentifier>", "key": "<keyData-as-hex>" } ] }
```

(The loader also accepts a raw exported keychain binary plist directly.)

### Key handling & anti-leak

The exported AES keys are sensitive, so they never touch an unencrypted or
synced location:

- `export-keys.sh` writes only into `~/Library/Application Support/ac-dc`
  (inside FileVault, **not** iCloud-synced `~/Desktop`/`~/Documents`), `chmod
  600`, and `tmutil`-excluded so Time Machine won't back it up.
- It arms a **one-shot LaunchAgent** that deletes the export on your next macOS
  login and then removes itself — the keys survive exactly one Linux session.
- On Linux, [`scripts/import-keys-from-macos.sh`](scripts/import-keys-from-macos.sh)
  mounts the macOS volume **read-only** (`apfs-fuse`, FileVault-unlocked) and
  reads `keys.json` in place, so it never leaves encrypted storage in transit.
- Caveat: `rm` on an APFS SSD is **not** a cryptographic erase (copy-on-write +
  wear-leveling). FileVault-at-rest is the real protection; the auto-wipe is
  hygiene. Re-exporting is cheap, so we wipe aggressively.

### Transfer keys over Bluetooth (from any Mac)

Copying `keys.json` across a read-only APFS mount only works when Linux and
macOS are the **same** machine (a dual-boot). If your Mac is a **separate**
device, transfer the exported file over Bluetooth LE instead:

```sh
# On Linux:
ac-dc receive-key                 # starts a GATT server, waits for the Mac

# On the Mac (build the helper first — see macos/ac-dc-send/README.md):
ac-dc send-key                    # exports from the keychain, scans, connects, sends
```

`send-key` does its **own** keychain export, so `export-keys.sh` is not needed
on this path and the keys never touch the Mac's disk — they go from the keychain
straight into the encrypted BLE link. The same helper also has `ac-dc
export-keys` for the dual-boot flow; it uses `SecItemCopyMatching`, so unlike
`export-keys.sh` Path A it returns **every** key, without SIP disabled or Frida.

The BLE link is treated as **untrusted**. Each side generates an ephemeral
X25519 keypair and exchanges public keys over GATT; ECDH → HKDF-SHA512 gives a
session key that encrypts the payload with ChaCha20-Poly1305. Both ends then
print a **6-digit security code (SAS)** derived from a hash of the two public
keys:

```
  Security code (SAS): 042137     <- must be identical on both screens
```

**Compare the codes and only answer `y` on Linux if they match.** A
man-in-the-middle who substitutes public keys forces the two codes apart, so a
mismatch means abort. On a match, Linux decrypts and writes `keys.json` at mode
`0600`.

- Linux receiver: [`ac-dc receive-key`](src/transfer_ble.rs) (bluer GATT
  server). macOS sender: [`ac-dc send-key`](macos/ac-dc-send/) (CoreBluetooth
  central), whose keychain export lives in
  [`KeychainExport.swift`](macos/ac-dc-send/Sources/ac-dc/KeychainExport.swift). The transport-independent crypto/framing core is in
  [`src/transfer.rs`](src/transfer.rs) and is fully unit-tested.
- ✅ **Validated end to end on real hardware** (macOS 26.1 sender, Linux
  receiver): service discovery, the X25519 handshake, matching 6-digit SAS on
  both screens, and a 902-byte `keys.json` transferred in 2 frames and written
  on the Linux side. The Swift sender's session-key, SAS and framing are also
  pinned by unit tests to vectors generated from `src/transfer.rs`, so the two
  implementations cannot drift apart silently.
  This is an alternative to the read-only APFS mount above, not a replacement
  that's been proven end-to-end.

---

## Roadmap

- **M1 — discover + decrypt adverts** *(code complete; crypto unvalidated)*.
  Detect on Linux when a nearby same-Apple-ID device copies to the Universal
  Clipboard. BLE only; no content. `scan`, `capture`, `decrypt` subcommands.
- **M2 — pull clipboard content** *(scaffolded; blocked on keys + AWDL test)*.
  After a copy is announced, act as the companion-link client: discover
  `_companion-link._tcp` over mDNS, run Apple's **Pair-Verify** handshake
  (Curve25519 ECDH authenticated with the Ed25519 long-term keys from the
  `RPIdentity-SameAccountDevice` identities), derive session keys, and pull the
  clipboard payload (**ChaCha20-Poly1305**, **OPACK**-encoded), then `wl-copy`.
  - **Built and offline-tested now:** `opack.rs`, `tlv8.rs`, and `companion.rs`
    (HKDF-SHA512, the ChaCha20-Poly1305 content channel, ContinuityPacket
    framing, and the Pair-Verify M1–M4 client with a full local loopback test).
    The `discover` subcommand is **live-runnable** with no keys.
  - **Still missing:** the real `RPIdentity` key export (only the BLE key is
    exported today); the TCP client that actually drives Pair-Verify against a
    device; and the clipboard-fetch OPACK request/response. None of the
    companion-link crypto is validated against a real device, and the
    Pair-Verify ChaCha nonce construction is an explicit TODO.
  - **Transport risk (the gating experiment):** Apple normally runs this over
    **AWDL**. The Asahi Wi-Fi driver (`brcmfmac`) has **no monitor mode**, so
    the open-source AWDL stack (OWL) won't run here. **Run `ac-dc
    discover` first** — if `_companion-link._tcp` resolves over the ordinary
    LAN while your devices are awake and nearby, M2 is tractable; if nothing
    ever resolves, the transport is AWDL-only and M2 becomes a driver-level
    project.
- **M3 — reverse direction (Linux → Apple)** *(advert builder only)*.
  `advertise.rs` builds our own Handoff advert bytes via `gcm::seal_truncated`
  (round-trip tested). Actually broadcasting via BlueZ LE advertising, and
  serving the companion-link pull, are documented stubs pending keys and a
  validated receive path.

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
