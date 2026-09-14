# M2 transport: creating the AWDL data interface (`awdl0`) on the BCM4378

**Status:** research + plan spike. Read-only investigation only — nothing here
was enabled, no iovar was set, no driver was patched or reloaded. This document
resolves one specific blocker for Route C in `docs/m2-awdl-plan.md` §7: **how the
`awdl0` netdev can be created on this machine's BCM4378, given that the
`awdl_if` iovar the reference bring-up script relies on is absent on our
firmware.** It proposes the mechanism, a minimal kernel-patch scope, the next
read-only probes, a guarded live-enable procedure, and the honest risks.

**Machine under test:** Apple MacBook (M1, `apple,j293` / `apple,t8103`),
Asahi/Arch. Wi-Fi = Broadcom **BCM4378** (`01:00.0`, `14e4:4378`) via
`brcmfmac` + `brcmfmac_wcc`. Firmware `brcmfmac4378b1-pcie.apple,*`, build tag
**`18.20.383.15.7.8.150`** (FWID `01-b37727a5`, 2023-05-13). Kernel booted
`7.1.13-1-1-ARCH`; installed `linux-asahi 7.1.13.asahi2-1` (modules for both
`7.1.13-1` and `7.1.13-2` present).

---

## 1. The blocker, restated precisely

Route C (§7 of the plan) established, on real hardware and read-only, that:

- the firmware's AWDL subsystem is present and iovar-driven (`awdl`,
  `awdl_doiovar_patch`, `wlc_awdl_attach`, `awdl_psf_dwell` in the image);
- `getstr cap` includes `awdl`;
- the shipped Asahi `brcmfmac` already exposes a userspace iovar/dcmd channel
  (nl80211 vendor command, Broadcom OUI `0x001018`, subcmd `BRCMF_VNDR_CMDS_DCMD`)
  — `brcmiovar.py` drives it and gets real firmware responses;
- most `awdl*` iovars from the iOS-driver catalog EXIST or are recognized;
- **but `awdl_if` returns `bcmerror` UNSUPPORTED** — the firmware does not
  implement that iovar on this 4378 build.

`awdl_if` is the iovar the `andreanicassio/brcmfmac-awdl` bring-up
(`~/Work/awdl-refs/awdl-up.sh`) uses to create the AWDL interface:

```sh
# from awdl-up.sh — wl_awdl_if2_t {int32 cfg_idx; int32 up; bssid[6]; if_addr[6]}
./brcmiovar.py -i $IF set awdl_if "<CFG>01000000002500ff9473<MAC>"
# firmware then emits WLC_E_IF (54) with role WLC_E_IF_ROLE_AWDL=7,
# and the patched driver materializes awdl0.
```

On the 4364 that project was developed against, `awdl_if` exists; on **our 4378
it does not**, so this exact path cannot create `awdl0` here. That is the whole
gap this document closes.

---

## 2. Key discovery: the two prior-art projects use *different* creation paths

The brief assumed both reference projects create `awdl0` via `awdl_if`. That is
wrong, and the correction is the crux of this spike. Reading the actual patch
(`omdrop-0001.patch`, saved in the spike scratchpad) shows:

### 2a. `andreanicassio/brcmfmac-awdl` (BCM4364/4377; T2) — via `awdl_if`
- Userspace `set awdl_if` → firmware creates a bsscfg → emits `WLC_E_IF` role 7.
- Kernel patch (`brcmfmac-awdl.patch`) recognizes role 7, creates an `awdl0`
  netdev (as `NL80211_IFTYPE_OCB`) via `brcmf_net_attach`, and forwards
  `WLC_E_AWDL_*` events to userspace.
- **Depends on `awdl_if` — dead end on our 4378.**

### 2b. `brentkearney/omdrop-awdl` (BCM4387; Asahi) — via `interface_create`
This project **does not use `awdl_if` at all.** Its patch 0001
("add AWDL interface creation for BCM4387") creates the interface through the
**generic, already-present `interface_create` iovar**, giving it a new interface
*type*:

```c
/* interface_create.c — mainline/Asahi already has this enum;
   value 2 is deliberately left empty by upstream. omdrop fills it: */
enum brcmf_interface_type {
	BRCMF_INTERFACE_TYPE_STA   = 0,
	BRCMF_INTERFACE_TYPE_AP    = 1,
	BRCMF_INTERFACE_TYPE_AWDL  = 2,   /* <-- omdrop adds this */
	BRCMF_INTERFACE_TYPE_NAN   = 3,
	BRCMF_INTERFACE_TYPE_P2P_GO= 4,
	...
};

int brcmf_cfg80211_request_awdl_if(struct brcmf_if *ifp, u8 *macaddr)
{
	return brcmf_cfg80211_request_if(ifp, BRCMF_INTERFACE_TYPE_AWDL, macaddr);
}
```

`brcmf_cfg80211_request_if()` is **existing Asahi driver code** — it is how the
driver already creates secondary STA/AP interfaces. It sends the `interface_create`
iovar using a version-negotiated struct (`brcmf_interface_create_v1/v2/v3`; v2 and
v3 carry an `iftype` field). Setting `iftype = 2` asks the firmware for an AWDL
bsscfg. The firmware answers with `BRCMF_E_IF` (event 54) ADD, and the driver's
normal add-if machinery builds the netdev — which omdrop then registers as
`awdl0`.

**Why this matters for the 4378:** `interface_create` is a *standard Broadcom
FullMAC iovar*, not an Apple-AWDL-specific one. It is present in the Asahi driver
and is the path the firmware already uses for its own secondary bsscfgs. It is
therefore very likely supported on the 4378 build even though `awdl_if` is not.
This is the realistic route to `awdl0` on this chip.

### 2c. Verdict on "iovar vs kernel patch"

**Both.** Neither the iovar alone nor a patch alone is sufficient:

- **The iovar path that works on 4378 is `interface_create` with `iftype = 2`
  (AWDL)** — *not* `awdl_if`. This is the mechanism to build on.
- **A kernel patch is still required**, because there is no `nl80211` interface
  type for AWDL, so `cfg80211`'s `add_virtual_intf()` cannot request it, and the
  stock driver will not turn a firmware-initiated AWDL `BRCMF_E_IF` into a
  registered netdev on its own. The patch is what drives `interface_create`
  (from a vendor command) and registers the resulting netdev as `awdl0`.

Sending iovars needs no patch (§1). Making `awdl0` *exist as a netdev* does.

---

## 3. Options for creating `awdl0` on the 4378

### Option A — `awdl_if` iovar + role-7 netdev patch (andreanicassio style)
**Rejected.** `awdl_if` is UNSUPPORTED on this firmware build. Even with the
role-7 kernel patch, there is no iovar to trigger the `WLC_E_IF` role-7 event.
Dead on arrival for 4378. (Keep only as the reference for *event/role* plumbing
ideas.)

### Option B — `interface_create` (iftype=2) + async netdev patch (omdrop style) — **RECOMMENDED**
Adapt omdrop's patch 0001 to our tree. Creation flow:

1. Userspace issues a vendor command (new subcmd `BRCMF_VNDR_CMDS_AWDL`,
   op `CREATE`).
2. Kernel calls `brcmf_cfg80211_request_awdl_if(ifp, NULL)` →
   `brcmf_cfg80211_request_if(ifp, BRCMF_INTERFACE_TYPE_AWDL, NULL)` → the
   existing `interface_create` iovar with `iftype = 2`.
3. Firmware creates the AWDL bsscfg and emits `BRCMF_E_IF` ADD.
4. The fweh event worker (holding no locks) registers the netdev as `awdl0`.

Pros: uses a firmware iovar that is very likely present (standard Broadcom
create path); reuses existing driver code; matches the newest, most complete
prior art (omdrop got receive-direction transfers working on 4387 this way).
Cons: needs a kernel patch and a driver reload; must be re-based onto our exact
tree; `interface_create` type-2 support on *this* 4378 build is probable but
still must be confirmed by probe (§5).

### Option C — no dedicated netdev; enable `awdl 1` on the primary bsscfg
**Not viable for our use case.** Even if `awdl 1` could be coaxed onto the
primary interface, the AWDL data path is delivered as ordinary 802.3 frames on
the AWDL *bsscfg's* ifidx via msgbuf flowrings; a companion-link TCP session
needs a real netdev with its own IPv6 link-local scope for `mdns-sd` and the
socket to bind to. Without a dedicated `awdl0` there is nowhere for the transport
to live. Might allow passive control-plane observation, but not data. Note also:
the reference bring-up shows `awdl 1` returns `BADOPTION` until `awdl_config` is
set on the AWDL bsscfg — i.e. the enable is designed to run against the created
bsscfg, not the primary. Reject for M2.

**Recommendation: Option B.** The rest of this document details it.

---

## 4. Minimal kernel-patch sketch (Option B, adapted from omdrop 0001)

Scope is small and confined to `drivers/net/wireless/broadcom/brcm80211/brcmfmac/`.
The essential subset to get `awdl0` to *appear* is one patch adapted from omdrop
0001; the data-plane and event patches are follow-ups.

**Files touched (creation only):**

| file | change |
|---|---|
| `interface_create.c` | add `BRCMF_INTERFACE_TYPE_AWDL = 2`; add `brcmf_cfg80211_request_awdl_if()` wrapper |
| `interface_create.h` | declare `brcmf_cfg80211_request_awdl_if()` |
| `cfg80211.c` | add `brcmf_awdl_add_vif()`, `brcmf_cfg80211_awdl_attach_pending()`, `brcmf_awdl_del_vif()` |
| `cfg80211.h` | `struct brcmf_cfg80211_info`: add `bool awdl_pending; char awdl_ifname[IFNAMSIZ];`; declare the three funcs |
| `fweh.c` | in `brcmf_fweh_handle_if_event()`, on `BRCMF_E_IF_ADD` call `brcmf_cfg80211_awdl_attach_pending(ifp)` |
| `vendor.c` | add `brcmf_cfg80211_vndr_cmds_awdl_handler()` + register it in `brcmf_vendor_cmds[]` |
| `vendor.h` | add `BRCMF_VNDR_CMDS_AWDL` to `enum brcmf_vndr_cmds`; add `enum brcmf_vndr_awdl_op {CREATE, DESTROY}` |

**The one non-obvious constraint (must be preserved when adapting):**
`nl80211` dispatches vendor commands **holding the wiphy mutex**
(`NL80211_CMD_VENDOR` sets `NEED_WIPHY` without `NO_WIPHY_MTX`). Registering a
netdev from inside the vendor command re-enters that mutex through the netdev
notifier chain (`register_netdev` → RTNL → `cfg80211_netdev_notifier_call` →
wiphy mutex, already held) — a self-deadlock that wedges the RTNL for the whole
system and needs a reboot to recover. omdrop's fix, which we must keep:

- `brcmf_awdl_add_vif()` **only** arms the vif event and asks firmware
  (`interface_create`), then returns immediately — it does **not** attach the
  netdev.
- The vif event must be armed *before* the iovar (`brcmf_cfg80211_arm_vif_event`),
  because `brcmf_notify_vif_event()` is what links the `brcmf_if` to the vif;
  otherwise `ndev->ieee80211_ptr` is never set and cfg80211 ignores the netdev.
- The netdev is registered later by `brcmf_cfg80211_awdl_attach_pending()`,
  called from the fweh worker (`brcmf_fweh_handle_if_event`), which runs on the
  system workqueue holding **neither** RTNL nor wiphy mutex — the only safe
  context for `brcmf_net_attach()` on an interface created outside
  `add_virtual_intf()`.
- Pass `NULL` MAC to `interface_create` (supplying a MAC at create time kills the
  firmware control channel ~2 s later on 4387); set the AWDL address afterwards
  via `cur_etheraddr`. AWDL BSSID is the fixed `00:25:00:ff:94:73`.
- The vif is allocated as `NL80211_IFTYPE_STATION` for cfg80211 bookkeeping; the
  firmware `iftype` is what makes it AWDL. (andreanicassio used `OCB` instead;
  either can work — STATION is simpler and is what the newer 4387 work settled on.)
- Teardown: `brcmf_fil_bsscfg_data_set(ifp, "interface_remove", NULL, 0)` and wait
  for `BRCMF_E_IF_DEL`.

**Follow-up patches (needed for data/discovery, not for `awdl0` to exist),
mapped from the omdrop series:**
- Forward AWDL firmware events to userspace as nl80211 vendor events (AW windows,
  role changes, action-frame RX/TX status). *Caveat:* the 4378 firmware's AWDL
  event/iovar surface must be re-probed (§5) — the `WLC_E_AWDL_*` codes and
  struct sizes were reverse-engineered on 4364/4387 and are build-specific.
- omdrop patch **0007** — translate AWDL data frames at the `awdl0` boundary
  (LLC/SNAP + Apple OUI encapsulation).
- omdrop patch **0008** — tolerate txstatus for a freed flowring (NULL-deref
  fix); a standalone bug fix worth taking regardless.
- omdrop patches **0009–0011** — action-frame instrumentation. Needs a debug
  build; the data-path gate relies on counting per-frame `awdl txstatus` lines,
  so do not strip the debug build.

**Packaging:** follow omdrop's DKMS-with-failsafe model — install the patched
module to `updates/dkms/` so `depmod` prefers it but the stock in-tree
`brcmfmac` remains as a fallback. A broken out-of-tree build then loses AWDL,
never the network. Pin the kernel tag (omdrop pins `asahi-7.1.13-2`, which
matches our installed `linux-asahi 7.1.13.asahi2-1`).

---

## 5. Next read-only probes to run (disambiguate before writing any patch)

All are **read-only** (`getstr` / `get`, i.e. `WLC_GET_VAR`, plus `probefile`
which only GETs). None sets an iovar, enables AWDL, patches, or reloads the
driver. They need `CAP_NET_ADMIN` (sudo, in a separate terminal per the machine's
sudo note). Run from a copy of `~/Work/awdl-refs/`.

1. **Reconfirm the AWDL capability bit (baseline):**
   ```sh
   sudo ./brcmiovar.py -i wlan0 getstr cap 2048 | tr ' ' '\n' | grep -i awdl
   ```

2. **THE decisive probe — does `interface_create` exist and at what version?**
   This is the iovar Option B rides on. A GET performs the driver's own version
   negotiation query and returns the supported struct/version:
   ```sh
   sudo ./brcmiovar.py -i wlan0 get interface_create 64
   ```
   Expected: a non-error return (a small struct whose leading `ver` field is 1, 2
   or 3). **A v2 or v3 return is the green light** — those carry the `iftype`
   field we need to set to 2. If it returns UNSUPPORTED, Option B is also blocked
   and we fall back to Route B (OWL on a dongle).

3. **Confirm `awdl_if` really is absent (records the negative for the doc):**
   ```sh
   sudo ./brcmiovar.py -i wlan0 probe awdl_if interface_create interface_remove
   ```
   Expect `awdl_if -> absent (UNSUPPORTED)`, `interface_create -> EXISTS`.

4. **Re-map the whole `awdl*` surface on the 4378 build** (the RE'd catalog is
   4364-specific; ours is FW `18.20.383.15.7.8.150`):
   ```sh
   sudo ./brcmiovar.py -i wlan0 probefile iovars-awdl.txt
   ```
   Note which of `awdl`, `awdl_cap`, `awdl_config`, `awdl_sync_params`,
   `awdl_chan_seq`, `awdl_peer_op`, `awdl_stats`, ... report EXISTS vs
   UNSUPPORTED. `awdl_peer_op` presence matters a lot: omdrop needed
   `awdl_peer_op ADD` for any unicast TX to avoid `FW_TOSSED`.

5. **Confirm the AWDL handler is reachable (near-zero risk GET):**
   ```sh
   sudo ./brcmiovar.py -i wlan0 get awdl 4
   sudo ./brcmiovar.py -i wlan0 get awdl_cap 64
   ```
   Distinguish error classes with `bcmerrorstr` — the tool already reads it back
   because brcmfmac collapses every firmware error to `-EBADE`.

6. **(After a future, guarded create only — not now)** verify the netdev/bsscfg
   actually appeared and events fire, read-only:
   ```sh
   iw dev; ip -br link | grep -i awdl
   sudo dmesg | grep -iE 'brcmfmac|E_IF|bsscfg|awdl'
   ```

**Decision gate:** if probe 2 returns v2/v3 EXISTS, proceed to build the Option B
patch. If it returns UNSUPPORTED, stop pursuing 4378-native creation and fall
back to Route B (dongle + OWL); do not spend patch effort.

---

## 6. Guarded live-enable procedure (proposed; do NOT run in this spike)

Enabling AWDL time-shares the radio off the AP channel and, with a dense channel
sequence, can make `wlan0` unusable or wedge the firmware (observed in the
reference lab log). So this runs **only** deliberately, with the user forewarned,
a fallback network in place, and a one-command recovery ready.

**Preconditions**
- A fallback network that does **not** depend on `wlan0`: wired USB-Ethernet, or
  tether. Confirm it carries traffic *before* touching AWDL.
- The Option B patched `brcmfmac` installed via DKMS-with-failsafe (§4), so a bad
  build or a reboot returns to stock automatically.
- A second terminal already holding sudo (the machine's sudo needs a separate
  terminal), so recovery does not depend on the possibly-degraded link.
- `awdl-down.sh` staged and tested to run without network access.

**Sequence (mirrors the reference `awdl-up.sh`, but create via the vendor
command, not `awdl_if`)**
1. Bring up the fallback net; verify connectivity.
2. Create the interface: issue the `BRCMF_VNDR_CMDS_AWDL` CREATE vendor command
   (via the small helper the patch ships). Wait for `awdl0` to appear
   (`ip link show awdl0`); **do not assume the vendor command created it
   synchronously** — the netdev is registered asynchronously by the fweh worker.
3. `ip link set awdl0 up`.
4. On the AWDL bsscfg (`brcmiovar.py -b <cfg>`), in the reference order:
   `awdl_config` first (else `awdl 1` → `BADOPTION`), then `awdl_af_hdr` /
   `awdl_af_rssi`, `awdl_sync_params`, `awdl_chan_seq`, `awdl_election_tree`,
   `awdl_opmode`, `awdl_extcounts`, `awdl_presencemode`, `awdl_aftxmode`,
   `awdl_psf_dwell`.
5. Use Apple's **sparse** channel sequence (infra channel in most slots, social
   channel 44 in a couple of slots, 6 in one) — a full 16/16 social sequence made
   Wi-Fi unusable in the lab, and (per omdrop) all-16 social also drops an idle
   Mac's replies after ~3 min, while encoding 0 schedules no 5 GHz TX at all.
   Keep the infra channel = the AP's current channel so `wlan0` keeps working.
6. `awdl 1` to enable. Leave the election self-metric at 0 (writing any non-zero
   self-metric makes this firmware elect itself master and desync from the peer).
7. Observe briefly (`awdlevents.py`, `awdl_stats` `datatx/datarx`, `iw dev`),
   then **tear down**.

**Recovery / `awdl-down`**
- `awdl 0` on the AWDL bsscfg; then destroy the interface
  (`BRCMF_VNDR_CMDS_AWDL` DESTROY → `interface_remove` → wait `BRCMF_E_IF_DEL`).
- If `wlan0` is wedged: `sudo modprobe -r brcmfmac_wcc brcmfmac brcmutil &&
  sudo modprobe brcmfmac` (drops Wi-Fi ~10 s; the fallback net covers it).
- If the RTNL is wedged (the deadlock §4 guards against — should not happen with
  the async attach, but if a bad patch reintroduces it): only a reboot recovers.
  This is precisely why the async-attach constraint is non-negotiable and why the
  DKMS failsafe + tested `awdl-down` are mandatory before the first enable.

**Safety gate:** the pass/fail check at every step is *does `wlan0` association
survive?* If it drops and the fallback net is not carrying traffic, stop and
recover before continuing.

---

## 7. Honest risks and the data-plane wall

- **The unicast data plane is the make-or-break unknown, and prior art hit a wall
  exactly here.** `companion-link` is a bidirectional unicast TCP session; the
  clipboard *pull* means **we initiate the TCP connection to the Apple device**.
  Both reference projects reach the AWDL control plane (PSF/MIF, TSF sync, peer
  discovery, mDNS) but struggle with addressed data:
  - `andreanicassio` (4364): no working unicast in either direction — firmware
    `datarx` stayed 0; the Apple peer never treated the Linux box as a reachable
    data peer.
  - `omdrop` (4387), further along: **receive works** (files arrive byte-exact
    from a Mac and an iPhone) once a firmware peer entry exists
    (`awdl_peer_op ADD`; without it every TX completes `tx_status 0x0003 =
    FW_TOSSED`). But **the send / we-initiate direction never worked**: "no file
    has ever reached an Apple device from here," and connections to a Mac fail at
    TCP with zero SYN-ACK (port 8770). That failing direction is the one a
    companion-link pull needs.
  So the realistic near-term outcome on 4378 is: `awdl0` exists, discovery/mDNS
  resolves, inbound may work, **but our outbound TCP connect to the Apple device
  may not** — potentially dead-ending the pull. This must not be overclaimed.
- **One angle unique to us:** part of the wall is waking a passive Apple receiver,
  which AirDrop does with a **BLE trigger** — and `ac-dc` already has the BLE side
  (M1). Whether a *same-Apple-ID companion-link* device (possibly already awake)
  behaves like an AirDrop receiver here is untested and worth probing, but is not
  a given.
- **`interface_create` type-2 support on this exact 4378 build is probable, not
  proven.** Probe 5.2 settles it before any patch work.
- **4378 AWDL ABI drift.** The `awdl*` iovar layouts, `WLC_E_AWDL_*` event codes,
  and struct sizes were RE'd against 4364/4387 firmware. Our build is FW
  `18.20.383.15.7.8.150` (2023) — re-probe (§5.4) before trusting any offset.
- **Destabilizing the user's only network.** Enabling AWDL can drop or wedge
  `wlan0`. Strictly gated (§6); never during a read-only spike.
- **Kernel patch + reload required.** Sending/​probing iovars is patch-free;
  making `awdl0` exist and seeing events is not. A reload drops Wi-Fi ~10 s.
- **Prior art is AI-assisted and unmerged.** Asahi's LLM policy forbids such
  contributions, so these patches will not appear upstream; treat them as
  reference and re-derive cleanly. The instrumentation (debug build) is load-
  bearing for diagnosing the data path — keep it.
- **Everything in §4 of the main plan still applies** regardless of transport:
  RPIdentity key export, the companion-link TCP socket driver, and
  mDNS-over-`awdl0`. Route C only changes *how `awdl0` is born*.

---

## 8. Bottom line

- **How `awdl0` is created on 4378:** via the **`interface_create` iovar with
  `iftype = 2` (AWDL)** — the omdrop/4387 mechanism — **plus a minimal kernel
  patch** to drive it from a vendor command and register the netdev
  asynchronously. **Not** via `awdl_if`, which is UNSUPPORTED on this firmware.
- **Minimal patch scope:** adapt omdrop patch 0001 (7 files, all under
  `brcmfmac/`): add `BRCMF_INTERFACE_TYPE_AWDL = 2` + a request wrapper, a
  vendor CREATE/DESTROY subcommand, and async netdev attach from the fweh worker
  (to avoid the wiphy-mutex/RTNL deadlock). Data/discovery need the follow-up
  omdrop patches (event forwarding, data-frame encap 0007, txstatus fix 0008,
  instrumentation 0009–0011), with the 4378 event/iovar surface re-probed first.
- **Do this next (read-only):** probe `interface_create` (§5.2) — a v2/v3 EXISTS
  return is the go/no-go for the whole 4378-native route; re-map `iovars-awdl.txt`
  on the 4378 build (§5.4); confirm `awdl_if` absent and `awdl`/`awdl_cap`
  reachable (§5.3, §5.5).
- **Fallback:** if `interface_create` type-2 is unsupported, or the outbound data
  plane proves unreachable, fall back to Route B (OWL on an AR9271 dongle); all
  downstream M2 work carries over unchanged.
</content>
</invoke>
