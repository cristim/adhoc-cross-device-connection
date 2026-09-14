# M2 transport spike: reaching companion-link over AWDL on Linux

**Status:** research spike + plan. Nothing here has been built or validated
against a real device. This documents on-machine diagnostics, a feasibility
call between two routes to AWDL on Linux, and a concrete first-implementation
plan for the recommended route.

**Machine under test:** Apple MacBook Pro (13-inch, M1, 2020) — `apple,j293` /
`apple,t8103` — running Asahi/Arch, kernel `7.1.13-1-1-ARCH`.

---

## 1. Findings from on-machine diagnostics (read-only)

All commands were run read-only; no driver reload, no firmware/kernel change,
no `sudo`.

### 1.1 Wi-Fi chip: Broadcom **BCM4378** (not BCM4387)

`lspci` is unambiguous:

```
01:00.0 Network controller: Broadcom Inc. BCM4378 802.11ax Dual Band Wireless Network Adapter (rev 03)
01:00.1 Network controller: Broadcom Inc. BRCM4378 Bluetooth Controller (rev 03)
```

So the spike brief's guess of "likely BCM4387" is **wrong for this machine** —
BCM4387 is the M1 Pro/Max/M2 part. The plain M1 (t8103, j293) ships **BCM4378**.
This matters a lot for route A: nexmon/OWL support is per-chip, and 4378 ≠ 4387.

The matching firmware in `/lib/firmware/brcm/` is
`brcmfmac4378b1-pcie.apple,*.bin` (+ `.clm_blob`, `.txcap_blob`, `.txt`). The
tree also contains `brcmfmac4387c2-*` and `brcmfmac4388{b0,c0}-*` blobs, but
those are for other Apple models, not this one. Bluetooth firmware is
`brcmbt4378b1-apple,*` via the `hci_bcm4377` driver.

### 1.2 Driver stack

`lsmod` / `dmesg` module list:

```
brcmfmac_wcc   (Apple "WCC" firmware-loader variant)
brcmfmac       (fullmac driver)
brcmutil
cfg80211
```

This is the mainline `brcmfmac` fullmac driver plus the Apple-specific
`brcmfmac_wcc` firmware selector that Asahi/mainline added for Apple Silicon.
Firmware is Apple's signed blob loaded from the machine's own firmware store —
**not** a generic Cypress/Broadcom image.

### 1.3 Monitor mode: **NOT exposed. This is the blocker.**

`iw phy phy0 info` reports:

```
Supported interface modes:
    * IBSS
    * managed
    * AP
    * P2P-client
    * P2P-GO
    * P2P-device
```

There is **no `monitor` mode**, no active-monitor, and therefore no frame
injection surface at all. Bands present: 2.4 GHz (band 1, ch 1–14) and 5 GHz
(band 2), 802.11ax/HE capable. AWDL's "social" channels are **6 (2.4 GHz), 44
and 149 (5 GHz)** — the radio can tune all of them, but that is irrelevant
while the driver refuses monitor+inject.

There is **no `awdl0` interface** — only `wlan0` (managed, currently associated
to the user's FRITZ!Box AP). Confirmed via `iw dev` and `ip link`.

### 1.4 Corroboration

- Team already ran `ac-dc discover`: `_companion-link._tcp` does **not** appear
  on the ordinary Wi-Fi LAN → the service is AWDL-only here, exactly as
  `discover.rs`'s own doc-comment predicted.
- Upstream linux-wireless discussion confirms `brcmfmac` does not expose
  monitor mode generically, and that even where the firmware has minimal
  monitor support, the driver can't cleanly demux monitor frames (they arrive
  with `msg.ifidx == 0`, indistinguishable from the managed interface). No
  Asahi work exposes monitor mode for 4378.

**Bottom line of the diagnostics:** the built-in BCM4378 gives us *no* path to
monitor mode or injection today, so the open AWDL stack (OWL) cannot run on it
as-is.

---

## 2. Feasibility: Route A vs Route B

### Route A — patch `brcmfmac` + nexmon to expose monitor/injection/AWDL on the BCM4378

**Verdict: not viable in any reasonable timeframe. Likely a multi-month
research project, possibly infeasible. Do not pursue now.**

Why:

- **nexmon has no BCM4378 patch, and none for any Apple Silicon chip.** The
  repo's `patches/` dirs cover bcm4330/4335/4339/43430/43455(c0)/4356/4358/
  43582/43596/**4375b1**/**4389c1**/**4398d0**/6715b0 — i.e. its *newest* work
  is Samsung (4375), Pixel (4389/4398) parts. **No 4378, 4387, or 4388.**
  Writing a patch for a new chip is the full nexmon research effort *per chip*:
  extract the ROM, locate patch points, re-implement the d11 monitor/injection
  hooks against that exact firmware build. There is no starting point for 4378.
- **The firmware is Apple-signed.** nexmon's model is "patch the firmware image,
  reload it." On Apple Silicon the wl firmware is Apple's signed blob loaded
  via `brcmfmac_wcc`; patching and re-injecting it is far harder than on a
  Raspberry Pi and may be outright blocked by signature checks. This is
  unexplored territory.
- **brcmfmac ≠ the drivers nexmon targets.** nexmon's build system expects
  specific driver/firmware combos; the Asahi Apple port is none of them.
- **AWDL needs more than monitor mode.** OWL drives precise TSF-synchronised
  channel hopping via injection timing; a fullmac chip like this gives no such
  low-level control even if monitor were bolted on.

Realistically this is "port nexmon to a brand-new Apple-signed chip, then build
an AWDL-capable injection path on top" — a dissertation-scale effort with a real
chance of dead-ending on the firmware signature.

### Route B — monitor-mode USB Wi-Fi dongle + OWL

**Verdict: far more tractable. Days-to-weeks to first light, not months.
RECOMMENDED.** It sidesteps the BCM4378 entirely.

Why:

- **Use a known-good injection adapter.** OWL was developed and tested on the
  **Atheros AR9280** (`ath9k`, PCIe). The community-standard *USB* equivalent is
  the **AR9271** (`ath9k_htc`): Alfa AWUS036NHA, or **TP-Link TL-WN722N v1**
  (⚠ **v1 only** — v2/v3 are Realtek and will *not* do injection). These are the
  canonical monitor+injection cards.
- **The driver is mainline and ARM64-clean.** `ath9k_htc` is an in-tree module;
  firmware `htc_9271.fw` ships in linux-firmware. It runs on aarch64 Asahi, and
  the M1 has USB. No firmware patching, no signing problem.
- **OWL builds and runs on current Linux** and brings up an `awdl0` virtual
  interface via netlink, leaving the built-in Wi-Fi untouched. Because the
  dongle is a *second* radio (its own `phy`), `wlan0` stays on the user's
  infrastructure network the whole time — this also satisfies the "don't drop
  the user's network" constraint, since we never touch `brcmfmac`.

**Important caveat (accuracy):** the AR9271/`ath9k_htc` is **2.4 GHz only**, so
OWL on it can use **AWDL social channel 6 only**, not the 5 GHz channels (44/149)
that modern Apple devices usually prefer. First light is realistic on ch 6; if
2.4 GHz peering proves unreliable, a **dual-band `ath9k` PCIe card (AR9280/
AR9380)** is the fallback for 5 GHz — but that's not a USB option, so it's
awkward on a laptop. Start with AR9271/ch6.

**Honest risk on Route B:** OWL is an experimental research artifact from the
~2019–2021 AWDL reverse-engineering work and is effectively frozen. Whether it
still *peers with a 2026 iPhone/macOS* (the AWDL version may have drifted) is the
single biggest unknown — see §5.

---

## 3. First-implementation plan (Route B)

### Step 0 — Acquire hardware
Buy an **AR9271** USB dongle (Alfa AWUS036NHA, or TL-WN722N **v1** — verify the
chip; only v1 is Atheros). Ideal parallel option: an AR9280/AR9380 mini-PCIe
`ath9k` card in a USB enclosure for 5 GHz, but treat that as a stretch.

### Step 1 — Verify the dongle on this machine (read-only, then one root step)
1. Plug in; `iw list` should show a **new phy** whose interface modes include
   `* monitor`, and (in the same block) an injection-capable combination.
2. `ip link` shows a new `wlanN`.
3. Later, as root: `sudo iw dev wlanN set type monitor && sudo ip link set
   wlanN up`, confirm with `iw dev`. (Not done in this spike.)

### Step 2 — Build OWL
```
git clone --recursive https://github.com/seemoo-lab/owl
# deps: cmake, libnl-3-dev, libpcap-dev, libev-dev  (pacman equivalents on Arch)
cd owl && mkdir build && cd build && cmake .. && make
```
Watch for aarch64 build warnings; OWL is C and generally portable.

### Step 3 — Bring up AWDL on the dongle
```
sudo ./owl -i wlanN -c 6      # -c 6 because AR9271 is 2.4 GHz-only
```
OWL creates `awdl0` (IPv6 link-local) and integrates it via netlink. Free the
dongle from NetworkManager first (`nmcli dev set wlanN managed no`) so OWL owns
it. `wlan0`/`brcmfmac` is left alone → the user's network survives.

### Step 4 — The moment of truth: does companion-link resolve over awdl0?
With `awdl0` up and the iPhone/Mac awake and nearby, run `ac-dc discover`. AWDL
is bursty — Apple only powers it up when Handoff/AirDrop/Universal Clipboard is
active — so **trigger it by copying something on the iPhone**. Nice synergy: M1
already detects that copy over BLE, so the existing BLE "clipboard available"
event is a perfect wake signal to start browsing AWDL.

Success = `_companion-link._tcp` resolves with an IPv6 link-local address +
port scoped to `awdl0`.

### Step 5 — Wire the transport into ac-dc
Add a `companion-pull` subcommand (or extend `discover`) that:
1. waits for the BLE clipboard-available event (existing `scan` path),
2. browses `_companion-link._tcp` over `awdl0`,
3. resolves host / port / IPv6 (with the `awdl0` scope id),
4. TCP-connects and drives `companion.rs` Pair-Verify, then pulls content.

### How it plugs into the existing M2 scaffolding
- **`discover.rs`** already browses `_companion-link._tcp` via `mdns-sd`. Once
  `awdl0` exists this is the live experiment. Likely fix-ups: make sure
  `mdns-sd` actually enumerates/queries over `awdl0` (IPv6 link-local, group
  `ff02::fb`, with the correct scope id) and prefers the `awdl0` address. This
  is the one part of M2 that is testable the day the dongle arrives.
- **`companion.rs`** already has: `PairVerifyClient` (M1–M4 build/parse),
  `ContinuityPacket` framing, `ContentChannel` (ChaCha20-Poly1305 + HKDF-SHA512),
  and `PairingIdentity::load`. **What's missing is the socket driver** — there
  is currently *no* TCP loop. Need: connect to `addr:port`, write M1, read M2,
  write M3, read M4, then run the content channel. Packets are length-prefixed
  `ContinuityPacket`s over the stream, so the loop must frame on the 4-byte
  header (and remember EncryptedData advertises `body + 16` for the Poly1305
  tag). After the channel is up, send the OPACK clipboard-fetch request and
  decode the OPACK response.
- The `pv_nonce` label/nonce construction and the content-channel HKDF info
  strings (`"ServerEncrypt-main"` / `"ClientEncrypt-main"`) are **guesses** from
  the seemoo reference and are still `TODO(validate)` in the code — the first
  real handshake is what confirms or breaks them.

Route B work splits cleanly: the **dongle + OWL + discover** experiment and the
**RPIdentity key export** (§4.1, doable under macOS today) can proceed in
parallel, and the socket driver (§4.2) can be written and unit-tested against a
loopback before real hardware peers.

---

## 4. What else M2 needs beyond transport

Even with `awdl0` up and companion-link resolving, three pieces stand between us
and clipboard content:

### 4.1 Export the RPIdentity long-term keys
Today `macos/export-keys.sh` exports **only** the BLE Continuity AES keys
(keychain service `com.apple.continuity.encryption`). Pair-Verify needs the
**`RPIdentity-SameAccountDevice`** identity instead:
- our device's **Ed25519 signing key** (`ed_sk`) and **device IRK** (`dirk`),
- each peer device's **Ed25519 public key** (`edpk`).

`companion.rs::PairingIdentity::load` already expects exactly this JSON shape
(`ed_sk` / `dirk` / `peers[].edpk` hex). The missing work is a **new exporter**
that locates these in the macOS keychain (RemotePairing / Rapport /
`com.apple.private.alloy` / AuthKit-adjacent items — exact service name is an RE
task) and emits that JSON. Risk: these may be SEP-protected or non-exportable —
see §5.

### 4.2 The companion-link TCP client
The socket driver from Step 5 / §3: connect, stream-frame `ContinuityPacket`s,
run Pair-Verify M1→M4, then the ChaCha channel. After the channel opens, issue
the **pasteboard-fetch OPACK request** and decode the response into the actual
clipboard bytes, then `wl-copy`. None of this request/response shape is written
yet, and it is unvalidated.

### 4.3 TLS / long-payload path
Small clipboard text may come back inline in one OPACK/EncryptedData exchange,
but **large items (images, files, big text) are not inline**. Apple delivers
those over a separate bulk path (chunked/streamed EncryptedData packets and/or a
TLS-wrapped bulk channel, AirDrop-style). M2 will need: reassembly of
multi-packet OPACK payloads, and possibly a TLS session for bulk transfer. The
exact shape is unknown and needs a packet capture once transport works — treat
as a follow-up after inline text works.

---

## 5. Risks and unknowns (explicit)

**Route B / transport**
- **OWL vs 2026 AWDL (biggest unknown).** OWL is frozen ~2021. The AWDL protocol
  may have drifted; OWL may simply fail to peer with a current iPhone/macOS. No
  way to know without the hardware. This is the gating risk for the whole route.
- **Dongle sourcing.** Genuine AR9271 TL-WN722N **v1** is increasingly rare
  (v2/v3 are Realtek and useless here); must verify the chip revision at
  purchase. AWUS036NHA is a safer buy.
- **2.4 GHz-only limitation.** AR9271 restricts us to AWDL channel 6; modern
  Apple devices often prefer 5 GHz (44/149). Peering on ch 6 may be flaky;
  5 GHz needs a PCIe `ath9k` card, awkward on a laptop.
- **mDNS over awdl0.** IPv6 link-local scoping is fiddly; `mdns-sd` may not bind
  `awdl0` or honour the scope id correctly. May require a small custom responder.
- **AWDL is bursty + gated.** Apple only activates it on demand; we must trigger
  and time the browse against the BLE wake signal.

**M2 beyond transport**
- **RPIdentity export (biggest M2 unknown after transport).** Exact keychain
  location/service is unknown; the material may be SEP-protected or otherwise
  non-exportable, which would block Pair-Verify entirely.
- **Same-account trust.** Pair-Verify assumes an *existing* pairing. If the peer
  won't answer companion-link unless we're an established same-Apple-ID device
  in the way iCloud sets that up, our synthesized identity may be refused even
  with correct keys.
- **Unvalidated crypto.** `pv_nonce` label/nonce construction and the content
  HKDF info strings are reference-derived guesses (`TODO` in code); the content
  channel roles/counters are only loopback-tested.
- **Long-payload path** (§4.3) is entirely unknown pending a capture.

**General**
- Scope/ethics unchanged: this is interop against the user's *own* same-Apple-ID
  devices with keys the user exports from their *own* Mac.
- Do not reload `brcmfmac` or touch the built-in radio — that drops the user's
  network. Route B avoids the built-in radio entirely by design.

---

## 6. Recommendation

Pursue **Route B**. Buy an **AR9271** (`ath9k_htc`) USB dongle, stand up **OWL**
on it to get an `awdl0` interface, and use `ac-dc discover` (triggered by a copy
on the iPhone) to prove `_companion-link._tcp` resolves over `awdl0`. In
parallel, RE and extend the macOS exporter to dump the **RPIdentity** Ed25519
identity that `companion.rs` already expects, and write the companion-link TCP
socket driver against the existing Pair-Verify state machine. Route A (patching
`brcmfmac`/nexmon for the BCM4378) is a multi-month, possibly-infeasible
research project with no existing foundation for this chip and an Apple-signed
firmware obstacle — not recommended.
