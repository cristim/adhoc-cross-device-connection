# M2 transport spike: reaching companion-link over AWDL on Linux

**Status:** research spike + plan. Nothing here has been built or validated
against a real device. This documents on-machine diagnostics and a feasibility
call between three routes to AWDL on Linux, plus a concrete first-implementation
plan for the recommended route.

**Machine under test:** Apple MacBook Pro (13-inch, M1, 2020) — `apple,j293` /
`apple,t8103` — running Asahi/Arch, kernel `7.1.13-1-1-ARCH`.

> **Update (Route C supersedes A and B).** A later read-only spike found that the
> built-in BCM4378's Apple firmware *already contains the AWDL subsystem*, and
> that the Asahi `brcmfmac` driver already exposes a userspace path to send
> arbitrary firmware iovars (the same BCDC control channel macOS drives AWDL
> over). This reframes the whole problem: instead of reimplementing AWDL on a
> second radio in monitor mode (Route B) or patching firmware (Route A), we may
> be able to **turn on the firmware's native AWDL by mimicking the `awdl*`
> iovars macOS sends** — no monitor mode, no nexmon, no firmware patch, no
> second dongle. See **§7 "Route C"**, which is now the RECOMMENDED route.
> Routes A and B below are retained for context but are superseded by C.

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
(`ed_sk` / `dirk` / `peers[].edpk` hex). This exporter now exists — see
**`macos/RPIDENTITY.md`** and `macos/export-rpidentity.sh` +
`macos/inject-rpidentity.swift` + `macos/rpidentity-to-json.py`.

The keychain service is **`RPIdentity-SameAccountDevice`** (confirmed from
seemoo-lab `handoff-authentication-swift` `MacKeychainController.swift`): a
synchronizable generic-password whose value is an OPACK blob
`{ edPK: <32B pubkey>, dIRK: <16B> }`. Crucially the synced item holds only the
**public** `edPK`; the private `edSK` is **not** there. So the exporter takes
**approach (b)**: *generate our own* Ed25519 keypair, keep the private seed in
`rpidentity.json`, and `SecItemAdd` our public key as a new
`RPIdentity-SameAccountDevice` item so all same-account devices trust us (exactly
seemoo's `createNewRPIdentityItem`). Peer `edPK`s are read from the same
keychain (Path A `security` / Path B Frida on `rapportd`).

Approach (a) — exporting a real device's own `edSK` and impersonating it — is
almost certainly blocked because that private key is **SEP-protected /
non-exportable**. `macos/RPIDENTITY.md` documents a concrete **probe**
(`SecKeyCopyExternalRepresentation` returning `NULL` under a Frida hook on
`rapportd`) to confirm this on real hardware before relying on either path.
Remaining risk: even with a correctly injected item, a current peer may demand
more than key-presence to grant same-account trust — see §5.

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

## 6. Recommendation (SUPERSEDED — see §7)

> This section originally recommended Route B. A later spike (§7) found a more
> direct, lower-cost route — **Route C: drive the built-in firmware's native
> AWDL from the host over iovars** — and **Route C is now the recommended
> route.** Route B remains the best *fallback* if Route C's firmware AWDL
> stack turns out to be unreachable or too incomplete to peer.

*Original text, retained for the record:*

Pursue **Route B**. Buy an **AR9271** (`ath9k_htc`) USB dongle, stand up **OWL**
on it to get an `awdl0` interface, and use `ac-dc discover` (triggered by a copy
on the iPhone) to prove `_companion-link._tcp` resolves over `awdl0`. In
parallel, RE and extend the macOS exporter to dump the **RPIdentity** Ed25519
identity that `companion.rs` already expects, and write the companion-link TCP
socket driver against the existing Pair-Verify state machine. Route A (patching
`brcmfmac`/nexmon for the BCM4378) is a multi-month, possibly-infeasible
research project with no existing foundation for this chip and an Apple-signed
firmware obstacle — not recommended.

Note that the RPIdentity export (§4.1), the companion-link TCP socket driver
(§4.2), and the `_companion-link._tcp` mDNS-over-`awdl0` work are **transport-
independent**: they are needed no matter whether `awdl0` comes from OWL-on-a-
dongle (B) or from the built-in firmware (C), so that work proceeds in parallel
regardless of which transport wins.

---

## 7. Route C — firmware-native AWDL via host iovars (macOS-mimic) — **RECOMMENDED**

**Verdict: this is the route to pursue first.** It needs *no* second radio, *no*
monitor mode, *no* nexmon, and *no* firmware patch to *reach* the AWDL engine.
The premise, backed by the on-machine evidence and prior art below, is that the
BCM4378's own Apple firmware already implements the AWDL subsystem, and that
macOS turns it on by sending proprietary `awdl*` **iovars** down the ordinary
Broadcom **BCDC control channel** — the same channel the Asahi `brcmfmac` driver
already speaks and already exposes to userspace. So "enable AWDL" reduces to
"replay the iovar sequence macOS sends," host-side, against the radio the machine
already has.

This supersedes Route A (no firmware patching — we *use* Apple's firmware as-is)
and Route B (no dongle needed to bring up `awdl0`). **But note the honest wall in
§7.3/§7.7: two existing projects get the AWDL *control* plane working this way yet
neither gets the *data* plane (actual unicast transfer) working** — which is
exactly what a companion-link TCP pull needs. So C is recommended to *try first*
(it is cheap and prototypable today), with B retained as the fallback (§7.8).

### 7.1 Evidence 1 — the firmware contains the AWDL subsystem

All read-only (`strings`, no sudo, no driver/firmware/network changes).

Firmware selected for this board (`apple,j293`, confirmed via
`/proc/device-tree/compatible`) is the `brcmfmac4378b1-pcie.apple,*` family;
representative file inspected: `brcmfmac4378b1-pcie.apple,atlantisb.bin`. Its
embedded build tag:

```
<FW-TAG>4378b1-roml/config_pcie_perf_udm Version=18.20.383.15.7.8.150
        Date=2023-05-13T07:25:54Z FWID=01-b37727a5
```

AWDL-related plain strings present in the 4378 image:

```
awdl
awdl_doiovar_patch
awdl_psf_dwell
```

The **4364** sibling (`brcmfmac4364b3-pcie.apple,hanauma.bin`), less aggressively
packed, exposes more of the same subsystem:

```
awdl              awdl_doiovar_patch   awdl_psf_dwell
wlc_awdl_attach   wlc_awdl_aw_set      master_slice_mask_2g / master_slice_mask_5g
```

What these tell us:

- **`awdl_doiovar_patch`** — the name of the firmware's **AWDL iovar dispatch
  handler** (`do_iovar` for the `awdl` namespace). Its existence confirms AWDL is
  *driven by iovars*, i.e. host-issued BCDC commands. Linchpin of the route.
- **`wlc_awdl_attach`** — the WLC-layer AWDL module init: AWDL is a first-class
  firmware subsystem (`wlc_awdl_*`), same shape as `wlc_p2p_*`.
- **`wlc_awdl_aw_set`** — sets the AWDL **Availability Window** (channel-hop
  schedule); **`awdl_psf_dwell`** — the **Periodic Sync Frame** dwell timing;
  **`master_slice_mask_*`** — per-band channel-slice masks. These being firmware
  parameters means PSF tx and fine TSF are **handled in firmware** (§7.4).

**Honesty about compression.** These `.bin` images are largely compressed/packed
(the 1.37 MB 4378 image yields only ~8.3k printable strings; reclaim/relocated
sections read as garbage — `@CYBYA`, `Reclaim section %s: returned %d bytes`).
The AWDL strings above are only the ones in the **uncompressed loader/patch
region**. The *full* `awdl` **iovar name table** lives in the compressed body and
the firmware's internal dispatch table, **not as plain strings** — so `strings`
alone cannot enumerate the sub-iovar surface. We *confirmed the subsystem is
present and iovar-driven*; the exact sub-iovar catalog comes from RE (§7.2/§7.6),
not from this machine's strings. The board **nvram** `.txt`
(`...atlantisb-RASP-m.txt`) is pure RF/board calibration with **no AWDL knobs** —
consistent with AWDL being enabled at runtime by iovar, not nvram.

### 7.2 Evidence 2 — the Asahi `brcmfmac` already lets userspace send iovars (**the crux**)

This is what makes Route C *prototypable today without a kernel rebuild.* The
loaded module `/lib/modules/7.1.13-1-1-ARCH/.../brcmfmac/brcmfmac.ko` (vermagic
`7.1.13-1-1-ARCH … aarch64`) contains all of:

```
# nl80211 VENDOR-COMMAND passthrough to the BCDC control channel:
brcmf_cfg80211_vndr_cmds_dcmd_handler   brcmf_vndr_cmds   brcmf_vndr_dcmd_hdr
BRCMF_VNDR_CMDS_DCMD (enum: UNSPEC/DCMD/LAST)   oui_type / "%s: invalid OUI"
# the in-kernel iovar API, EXPORTED to other modules:
brcmf_fil_iovar_data_set  (__ksymtab_… -> EXPORT_SYMBOL)   brcmf_fil_iovar_data_get
# the BCDC control channel underneath:
brcmf_proto_bcdc_{set,query}_dcmd   brcmf_msgbuf_{set,query}_dcmd
```

Two userspace-reachable paths therefore exist in the shipped driver with **no
patch required to *send iovars***:

1. **nl80211 vendor command** — `NL80211_CMD_VENDOR` with the **Broadcom OUI
   `0x00:10:18`**, subcommand `BRCMF_VNDR_CMDS_DCMD` (=1), and a
   `NL80211_ATTR_VENDOR_DATA` blob shaped as `struct brcmf_vndr_dcmd_hdr {uint
   cmd; int len; uint offset; uint set; uint magic;}` + payload. An iovar SET is
   `cmd = WLC_SET_VAR (263)`, payload `"awdl\0"` + body; `WLC_GET_VAR (262)` to
   read. Nested attrs `BRCMF_NLATTR_LEN=1` / `BRCMF_NLATTR_DATA=2`; max request
   ≈ `BRCMF_DCMD_MAXLEN` 8192. **Recommended prototyping surface** — pure
   userspace, `CAP_NET_ADMIN`, no build. (`iw dev wlan0 vendor …`, a small libnl
   program, or the ready-made `brcmiovar.py` from the prior-art project below.)
2. **A tiny out-of-tree kmod** could instead call the exported
   `brcmf_fil_iovar_data_set(ifp, "awdl", buf, len)` directly — the literal call
   the driver makes internally.

Confirmed interface state (read-only): only `wlan0` (type `managed`, associated
to the user's AP); **no `awdl0`**; `iw` 6.17 present. The Apple vendor sub-module
`brcmfmac-wcc.ko` is only a firmware/feature selector (`feat_attach`) — no AWDL
logic of its own; the AWDL logic is all in firmware.

> **Crux answered: YES** — the shipped Asahi driver lets us send/probe arbitrary
> firmware iovars from userspace with no kernel rebuild. A kernel patch is needed
> only later, to materialize the `awdl0` **netdev** and forward AWDL **events**
> (see §7.5), not to reach the iovars.

**Important caveat on error reporting:** brcmfmac collapses every firmware error
to `-EBADE`, so a probe must read back `bcmerror`/`bcmerrorstr` to tell
UNSUPPORTED (iovar absent) from BADARG/NOTUP. Many `awdl*` iovars are
**bsscfg-scoped** — they must be prefixed `bsscfg:` / targeted at the AWDL bsscfg
index once it exists.

### 7.3 Prior art — TWO existing projects already do this (control plane works, data plane does not)

The parallel research turned up two GitHub projects doing exactly the
firmware-iovar (not monitor-mode) approach on Broadcom-Apple chips. **This is the
most important input to Route C** — it de-risks the "can we reach AWDL" question
and sharply defines where the wall is.

- **`andreanicassio/brcmfmac-awdl`** (BCM4364/4377/4378; T2 + Apple Silicon).
  The most on-point work. Ships `brcmiovar.py` (pure-Python nl80211 vendor-cmd
  iovar sender — our path #1 above, ready to use), `iovars-awdl.txt` (AWDL iovar
  names harvested from decompiling Apple's **iOS 26 AppleBCMWLAN DriverKit
  dext**), an extensive `NOTES.md` lab log (bring-up sequence, struct offsets,
  host/fw split, the data-plane failure), plus `awdl-up.sh`/`awdl-down.sh` and a
  small role-7 netdev patch `brcmfmac-awdl.patch`.
- **`brentkearney/omdrop-awdl`** (BCM4387, **Asahi**, kernel tag
  `asahi-7.1.13-2`). Eleven brcmfmac patches + DKMS; adds a
  `BRCMF_INTERFACE_TYPE_AWDL` and a `brcmf_cfg80211_vndr_cmds_awdl_handler` to
  create/manage `awdl0`; patches 0009–0011 instrument PSF/MIF action frames.

Both reach the same state: **AWDL control plane comes up** (firmware transmits
PSF/MIF, syncs TSF to a nearby iPhone/iPad/Mac, decodes peers; mDNS/service
discovery works) but **the AWDL data plane — actual unicast transfer — does not
work in either direction.** `NOTES.md`'s cross-verified conclusion (driving both
a Linux box and a macOS 26.5 Mac): firmware `datarx` stays 0; our IPv6/mDNS
multicast goes *out* on `awdl0`, but the Apple peer **advertises a link-local
address and never unicasts back** — it does not treat us as a reachable data
peer, so no unicast/flowring is established. Addressed data needs tight per-peer
availability-window scheduling + the peer confirming us as an active data peer +
(to wake a passive Apple receiver) the AirDrop **BLE** trigger — none achievable
by configuring the FullMAC firmware "blind" through iovars. This is the same
limitation OWL sidesteps with monitor-mode injection, which brcmfmac does not
offer. Neither project was submitted upstream (both AI-assisted; Asahi's LLM
policy forbids such contributions), so do not expect them in AsahiLinux trees.

*(Raw copies of `NOTES.md`, `iovars-awdl.txt`, `brcmiovar.py`, `awdl-up.sh`, and
the omdrop patch/README were saved to the spike scratchpad for reference.)*

**Other prior art:** seemoo-lab's "One Billion Apples' Secret Sauce" (Stute et
al., MobiCom '18, arXiv 1808.03156) is the authority on AWDL's *frame/TLV/timing*
semantics but **does not name the Broadcom `awdl*` iovars** — cite it for
protocol, not the iovar API. seemoo `owl`/`opendrop` are the monitor-mode
alternative (contrast, = our Route B lineage). The iovar names/structs are
reverse-engineered from the iOS 26 AppleBCMWLAN dext plus a leaked Broadcom
`wlioctl.h` AWDL fragment (FreshTomato GPL drop) — undocumented by Broadcom/Apple
and firmware-build-specific.

### 7.4 The `awdl` iovar surface and bring-up sequence (from the iOS 26 dext RE)

Names probed from Apple's iOS 26 driver (present on a BCM4364 build; **must be
re-probed on our 4378** — offsets/existence are build-specific):

```
awdl (enable u32)   awdl_if   awdl_cap   awdl_config   awdl_sync_params
awdl_chan_seq   awdl_election_tree   awdl_opmode   awdl_extcounts
awdl_presencemode   awdl_aftxmode   awdl_af_hdr   awdl_af_rssi   awdl_peer_op
awdl_advertisers   awdl_stats   awdl_psf_dwell   awdl_maxpeers   awdl_osoc_chan
awdl_min_rate   awdl_phycal_period   awdl_dfsp_cfg/_ucsa   awdl_payload
awdl_afs_pload   awdl_oob_af[_auto]   awdl_ranging[_config]/_ftm_ranging_config
```

Selected RE'd struct layouts (from `NOTES.md`, cross-checked vs. leaked
`wlioctl.h` and probed live on 4364):

- **`awdl_if`** = 20 B `{int32 cfg_idx; int32 up; ether_addr bssid; ether_addr
  if_addr;}`; AWDL BSSID fixed `00:25:00:ff:94:73`. SET triggers `WLC_E_IF`
  (event 54) with role `WLC_E_IF_ROLE_AWDL = 7`.
- **`awdl_sync_params`** 36 B (`aw_period`=16 TU, `af_period` Apple ≈110 TU, ext
  counts, `presence_mode`).
- **`awdl_chan_seq`** header `{u8 count-1, u8 enc, u8 dup, u8 step, u16 fill}` +
  16 slots. `enc=0` 1-byte channel (0 = infra channel); `enc=2` big-endian D11AC
  chanspecs (5 GHz ch44 = `0xd02c`, 2.4 GHz ch6 = `0x1006`).
- **`awdl_election_tree`** 42 B; **`awdl_af_hdr`** 10 B (category `0x7f`, Apple
  OUI `00:17:f2`); **`awdl_peer_op`** old format `{u8 version=0, u8 opcode(0 add/
  1 del/2 info/3 upd), ether_addr, u8 mode}`.
- **Events:** `WLC_E_AWDL_AW`=96, `WLC_E_AWDL_ROLE`=97, `WLC_E_AWDL_EVENT`=98
  (subtypes RX_ACT_FRAME/PEER_STATE/INTERFACE_STATE), action frames as
  `WLC_E_ACTION_FRAME_RX`=75, tx status `WLC_E_ACTION_FRAME_COMPLETE`=60.

**Bring-up order** (from the dext): `awdl_if` SET on the primary interface →
firmware creates a bsscfg + emits `WLC_E_IF` role 7 → on that bsscfg set
`awdl_config` (must precede enable — `awdl 1` returns `BADOPTION` otherwise),
then `awdl_af_hdr/_rssi`, `awdl_sync_params`, `awdl_chan_seq`,
`awdl_election_tree`, `awdl_opmode`, `awdl_extcounts`, `awdl_presencemode`,
`awdl_aftxmode`, `awdl_psf_dwell`, misc → **`awdl` = 1** to enable → set AF tx
mode. Peers via `awdl_peer_op`; discovered peers read back via `awdl_advertisers`.

### 7.4b Host vs. firmware — responsibility split (authoritative, from the dext RE)

- **Firmware (autonomous, timing-critical):** AW timing, channel hopping, **TSF
  sync**, election-tree bookkeeping, transmitting PSF/MIF action frames at the
  right time, the peer table, power save. (Consistent with our firmware evidence:
  `awdl_psf_dwell`, `wlc_awdl_aw_set`, `master_slice_mask_*`.)
- **Host (macOS `IO80211Family`/`AppleBCMWLANProximityInterface`, and what *we*
  drive over iovars):** parses received action frames (delivered as events), runs
  the AWDL state machine, *decides* channel sequence / election params and pushes
  them down via iovars; owns the `awdl0` netdev, IPv6 link-local, and all
  mDNS/`_companion-link._tcp` traffic. So a Linux port ≈ brcmfmac netdev/event
  plumbing + an OWL-like userspace daemon that no longer needs raw frames.

### 7.5 First experiment (describe only — do NOT run in this spike)

Goal: cheapest test of "does poking `awdl` do anything observable," with least
risk to the user's live Wi-Fi.

**Risk framing.** `wlan0` is the user's *only* network and is associated now. An
`awdl` iovar (esp. `awdl_chan_seq` / enable) can push the radio off the AP
channel and make the link unusable, or wedge the firmware (`NOTES.md` explicitly
saw an aggressive channel sequence make Wi-Fi "unusable" until disable). **So the
enable steps must NOT run while the user depends on this link, and never as part
of a read-only spike** — only later, deliberately, with the user forewarned and a
fallback network available.

Staged, safest first:

1. **Read-only probe (near-zero risk).** With `brcmiovar.py` (or equivalent),
   `WLC_GET_VAR "cap"` and check for `awdl`; then `GET_VAR "awdl"` / `"awdl_cap"`.
   A non-error return **proves the firmware AWDL handler is reachable from
   userspace on the 4378** — the single highest-value, lowest-risk signal.
   Distinguish errors via `bcmerrorstr` (brcmfmac maps all to `-EBADE`).
   **Re-probe the whole `iovars-awdl.txt` list to map which `awdl*` exist on our
   4378 build** (they differ from the 4364).
2. **Enable, observe, disable — quickly, with a fallback net up.** Follow the
   §7.4 order to `awdl 1`, then immediately watch and run `awdl-down`. Observe: a
   new `awdl0`/bsscfg netdev (needs the role-7 kernel patch to actually appear —
   without it the firmware makes the bsscfg but stock brcmfmac ignores role 7);
   `WLC_E_AWDL_*`/`WLC_E_IF` events; whether `wlan0` association survives (the
   pass/fail safety gate); `awdl_stats` `datatx/datarx`.
3. **Only if 1–2 are clean:** program channel sequence + election params, add a
   peer (`awdl_peer_op`), and look for received peer AFs / `awdl_advertisers`
   populating, with an Apple device nearby and AWDL triggered.

Success ladder: (a) iovar reachable → (b) `awdl0` appears → (c) frames tx'd →
(d) peer seen / TSF sync → (e) `_companion-link._tcp` resolves over `awdl0` →
(f) **unicast TCP actually connects** (the step both prior projects fail at).

### 7.6 What we still have to reverse-engineer / build

- **Re-probe the `awdl*` catalog + struct offsets on the 4378** FW
  `18.20.383.15.7.8.150` — the RE'd layouts are 4364-specific.
- **A brcmfmac patch** to attach the `awdl0` netdev on `WLC_E_IF` role 7 and
  route `WLC_E_AWDL_*` events to userspace (the `andreanicassio` role-7 patch or
  the `omdrop` `interface_create` approach are starting points — but both are
  AI-assisted and unmerged, so treat as reference, re-derive cleanly).
- **The data-plane unblock (the real problem, §7.7).**
- The exact `brcmf_vndr_dcmd_hdr` field encoding/`magic`/endianness — confirm
  against brcmfmac source, don't guess.

### 7.7 Risks / unknowns specific to Route C

- **Data plane may never work (TOP risk, now evidenced).** Two independent
  projects bring up AWDL control + discovery but get **no working unicast data
  path** on brcmfmac — the FullMAC firmware won't schedule per-peer data windows
  "blind," and the Apple peer won't unicast to a device it hasn't confirmed as a
  data peer. **companion-link is a bidirectional unicast TCP session**, so this
  could dead-end our whole use case at "`awdl0` up, mDNS resolves, TCP never
  connects." This is the make-or-break unknown. *One angle unique to us:* the
  wall partly involves the **AirDrop BLE trigger** that wakes a passive receiver
  — and `ac-dc` **already has the BLE side** (M1). Whether the companion-link
  path (same-Apple-ID, possibly already-awake device) behaves like AirDrop here
  is untested and worth probing, but must not be overclaimed.
- **Destabilizing the user's live Wi-Fi** — a bad `awdl` iovar can drop `wlan0`
  or wedge firmware. Strictly gated (§7.5); never during a read-only spike.
- **Iovar surface is compressed / build-specific** — must be re-probed on 4378.
- **Needs a kernel patch for the netdev/events** — sending iovars is patch-free,
  but seeing `awdl0` and receiving events is not.
- **Firmware/version drift** — 4378 FW is 2023; the `awdl` ABI may differ from
  the 4364 build the RE was done against.
- **Still needs everything in §4** — RPIdentity export, companion-link TCP
  driver, mDNS-over-`awdl0`; Route C only changes *how `awdl0` is born*.

### 7.8 Why C first, B as fallback

C's advantages: **zero hardware cost**, uses the **5 GHz-capable built-in radio**
(AWDL social ch 44/149 that modern Apple devices prefer — Route B's AR9271 is
2.4 GHz/ch-6 only), **prototypable today** via the userspace iovar path (§7.2),
and there is **working reference code** (`brcmiovar.py`, `iovars-awdl.txt`, the
bring-up sequence) to start from. Its gating risk is the **data plane** (§7.7),
which is *cheaply testable*: step 7.5.1 alone (a read-only probe) validates the
premise for the price of a couple of vendor commands, and the prior art tells us
where the wall is before we spend a cent. **If Route C dead-ends at the data
plane, fall back to Route B** (OWL on an AR9271 dongle, which has a real
monitor/injection data path) — and every downstream M2 piece (RPIdentity export,
TCP driver, mDNS) carries over unchanged.
