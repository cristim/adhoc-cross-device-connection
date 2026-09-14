# brcmfmac AWDL patch for the BCM4378 (`awdl0` on Apple M1 / Asahi)

A minimal `brcmfmac` kernel patch that creates an **`awdl0`** netdev on this
machine's **Broadcom BCM4378**, plus scripts to build it, bring AWDL up, and
tear it down. This is the M2-transport "Route C" work from
[`docs/m2-awdl-plan.md`](../../docs/m2-awdl-plan.md) and
[`docs/m2-awdl-4378-bringup.md`](../../docs/m2-awdl-4378-bringup.md): drive the
firmware's *own* AWDL engine over iovars instead of reimplementing AWDL in
userspace.

> **This is a first draft that has NOT been built or loaded.** The patches are
> verified only to *apply cleanly* to the matching Asahi source (see below).
> Everything about the firmware's runtime behaviour on the 4378 is
> **UNVERIFIED-until-built**, and there are two known downstream walls (§ Honest
> unknowns) that may stop this short of a working clipboard pull. Read the whole
> file before you load anything.

---

## Machine / versions this targets

| | |
|---|---|
| Board | Apple MacBook, `apple,j293` / `apple,t8103` (M1) |
| Wi-Fi chip | **BCM4378** (`14e4:4378`), `brcmfmac` + `brcmfmac_wcc` |
| Firmware | `brcmfmac4378b1-pcie.apple,*`, build `18.20.383.15.7.8.150` (2023-05-13) |
| Kernel (running) | `7.1.13-1-1-ARCH` aarch64 |
| Package | `linux-asahi 7.1.13.asahi2-1` → source tag **`asahi-7.1.13-2`** |

The prior art is **`brentkearney/omdrop-awdl`** (BCM4387, same Asahi kernel).
Its driver source at `asahi-7.1.13-2` is **byte-identical** to ours — the
BCM4378-vs-4387 difference is *firmware/runtime*, not driver source — so these
patches are omdrop's diff hunks unchanged, with commit messages re-written for
the 4378 and every 4378-specific assumption flagged UNVERIFIED.

---

## The mechanism (why `interface_create`, not `awdl_if`)

The reference bring-up on T2 Macs creates the AWDL interface with the `awdl_if`
iovar. **On this 4378 firmware `awdl_if` is UNSUPPORTED** (confirmed read-only
via the nl80211 vendor passthrough). What *does* exist is the generic Broadcom
**`interface_create`** iovar, and a live probe confirmed it returns a **v3
struct** (`brcmf_interface_create_v3`, which carries an `iftype` field).

So the creation path is: ask `interface_create` for `iftype = 2`
(`BRCMF_INTERFACE_TYPE_AWDL`, a slot upstream deliberately leaves empty). The
firmware makes an AWDL bsscfg and emits `BRCMF_E_IF_ADD`; the patch registers
the resulting netdev as `awdl0`. `nl80211` has no AWDL interface type, so this
cannot go through `add_virtual_intf()` — it is driven from a **vendor command**
instead.

### The load-bearing constraint: register the netdev ASYNCHRONOUSLY

`nl80211` dispatches vendor commands **holding the wiphy mutex**. Registering a
netdev from inside the vendor handler re-enters that mutex through the netdev
notifier chain (`register_netdev` → RTNL → `cfg80211_netdev_notifier_call` →
wiphy mutex, already held) — a **self-deadlock that wedges the RTNL for the
whole system and needs a reboot to recover**. The patch therefore:

- `brcmf_awdl_add_vif()` (vendor CREATE) only **arms the vif event and asks
  firmware**, then returns. It does *not* attach the netdev.
- The netdev is registered later by `brcmf_cfg80211_awdl_attach_pending()`,
  called from the **fweh event worker** (`brcmf_fweh_handle_if_event()` on
  `BRCMF_E_IF_ADD`), which holds neither RTNL nor wiphy mutex — the only safe
  context. (Teardown is symmetric: patch 0004 defers the netdev removal to the
  same worker to avoid the mirror-image RTNL assert on destroy.)

**Consequence for userspace:** the CREATE vendor command returns *before*
`awdl0` exists. `awdl-up.sh` polls for the interface — do not assume it is there
on return.

---

## The patches

Apply in numeric order (0001 → 0004). All four touch only
`drivers/net/wireless/broadcom/brcm80211/brcmfmac/`.

| patch | files | what it does |
|---|---|---|
| `0001-…-interface-create.patch` | `interface_create.{c,h}`, `cfg80211.{c,h}`, `fweh.c`, `vendor.{c,h}` | `BRCMF_INTERFACE_TYPE_AWDL=2` + `brcmf_cfg80211_request_awdl_if()`; `brcmf_awdl_add_vif`/`attach_pending`/`del_vif`; async attach from fweh; `BRCMF_VNDR_CMDS_AWDL` CREATE/DESTROY vendor subcommand |
| `0002-…-netdev-ops.patch` | `core.{c,h}`, `cfg80211.c` | AWDL-specific `net_device_ops` so `ip link set awdl0 up` does **not** run station bring-up against the AWDL bsscfg (which wedges the firmware and would take `wlan0` down) |
| `0003-…-usable-create-args.patch` | `interface_create.c` | send v3 `interface_create` with flags `0x1a` + explicit BSSID + `if_index 2` (as module params) so the AWDL iovars become usable |
| `0004-…-rtnl-safe-teardown.patch` | `cfg80211.c` | make DESTROY ask firmware and return; the fweh worker removes the netdev — avoids an RTNL-assert deadlock |

**Verification status:** all four **apply cleanly and in sequence** to the Asahi
`brcmfmac` source at `asahi-7.1.13-2` (checked: no fuzz, no offsets, no
rejects). They have **not** been compiled or loaded. Provenance: derived from
`brentkearney/omdrop-awdl` (GPL-2.0-only, AI-assisted, never submitted upstream
per Asahi's LLM policy); authorship preserved in each patch header.

Not included (out of scope for "make `awdl0` exist", available in omdrop if you
go further): 0003/0006 firmware error-code + RAM-snapshot vendor ops, 0007 data-
frame encapsulation, 0008 txstatus NULL-deref fix, 0009–0011 action-frame
instrumentation.

### What is 4378-specific and UNVERIFIED

- **Firmware accepts `interface_create` iftype=2 and emits `BRCMF_E_IF_ADD`** —
  probable (the iovar exists and returns v3), not proven, until built + loaded.
- **The create magic values** (`awdl_create_flags=0x1a`, `awdl_if_index=2`,
  `awdl_bssid=00:25:00:ff:94:73`) were tuned on 4387. They are **module
  parameters** (writable `0644`) so you can sweep them *without rebuilding*:
  ```
  sudo modprobe brcmfmac awdl_create_flags=0x1a awdl_if_index=2
  ```
  If create succeeds but AWDL iovars return `BCME_NOMEM`/`BADARG`, try
  `awdl_create_flags=0x02` (MAC-only), other `awdl_if_index`, or a different
  BSSID.
- **The `awdl_*` iovar names, sizes and the enable order** used by `awdl-up.sh`
  were reverse-engineered on 4364/4387. Re-probe them on this build with
  `~/Work/awdl-refs/brcmiovar.py probefile iovars-awdl.txt` if a step errors.

---

## Step by step

### 0. Before you touch anything — safety preconditions

Loading the module briefly drops Wi-Fi, and enabling AWDL time-shares the radio
off the AP channel. A bad channel sequence made Wi-Fi unusable in the reference
lab. So:

1. **Have a fallback network that does not depend on `wlan0`** — USB-Ethernet or
   a phone tether — and confirm it carries traffic *first*.
2. **Open a second terminal already holding `sudo`** (this machine's sudo needs
   a separate terminal), so recovery does not depend on the degraded link.
3. Know the recovery command (below) before you start.

### 1. Build (only builds — never loads)

```bash
cd kernel/brcmfmac-awdl-4378
./build.sh
```

`build.sh` fetches the Asahi kernel source at `asahi-7.1.13-2` (a large shallow
clone; set `KSRC=/path/to/linux` to reuse an existing checkout), applies the
four patches, and compiles **only** `brcmfmac.ko` against your *running*
kernel's headers (`/lib/modules/$(uname -r)/build`), so the vermagic matches. It
prints the `.ko` path and the manual load/unload commands — **it does not load
anything.** Needs `linux-asahi-headers`, `base-devel`, `git`.

### 2. Load (deliberate, manual, risky — you run this)

With the fallback network up and a second sudo terminal open:

```bash
KO=kernel/brcmfmac-awdl-4378/build/linux/drivers/net/wireless/broadcom/brcm80211/brcmfmac/brcmfmac.ko
sudo modprobe -r brcmfmac_wcc brcmfmac brcmutil   # unload stock stack (~10s Wi-Fi drop)
sudo modprobe brcmutil
sudo insmod "$KO"                                  # patched brcmfmac
sudo modprobe brcmfmac_wcc
# confirm Wi-Fi reassociates before continuing
```

Nothing is installed under `/lib/modules`, so this is **temporary**: a reboot
restores the stock driver automatically.

### 3. Bring up AWDL

```bash
sudo ./awdl-up.sh          # creates awdl0 via the vendor CREATE command, then configures + enables
```

### 4. Verify

```bash
ip link show awdl0         # the netdev should exist and be UP
sudo dmesg | grep -iE 'awdl|E_IF|bsscfg'
ac-dc discover             # does _companion-link._tcp resolve over awdl0?
ac-dc pull                 # the actual goal (see wall #1 below)
```

### 5. Tear down / recover

```bash
sudo ./awdl-down.sh                                   # awdl 0 + vendor DESTROY
# revert to the stock driver entirely:
sudo modprobe -r brcmfmac_wcc brcmfmac brcmutil
sudo modprobe brcmfmac
```

If `wlan0` is wedged and commands hang: **reboot** — stock loads on next boot.
The async-attach design (patch 0001) and RTNL-safe teardown (patch 0004) exist
specifically so a *correct* build never wedges the RTNL; a reboot is the
backstop if a future edit reintroduces the deadlock.

---

## Honest unknowns

This may not reach a working clipboard pull. The two walls, from the plan docs
and the prior-art lab logs:

1. **The we-initiate data plane never worked in prior art.** A `companion-link`
   pull means *we* open the TCP connection to the Apple device. Both reference
   projects bring up the AWDL **control** plane (PSF/MIF, TSF sync, peer
   discovery, mDNS) but neither gets **our outbound unicast** working:
   `omdrop` on 4387 got *receive* working but "no file ever reached an Apple
   device from here" (TCP connect gets zero SYN-ACK); `andreanicassio` on 4364
   got no unicast either direction. So the realistic near-term outcome on 4378
   is: `awdl0` exists, discovery resolves, **but our TCP connect to the peer may
   not** — potentially dead-ending the pull. One angle unique to us: waking a
   passive receiver uses an AirDrop **BLE trigger**, which `ac-dc` already has
   (M1) — whether a same-Apple-ID, possibly-already-awake companion-link device
   behaves like an AirDrop receiver here is untested.
2. **Same-account trust.** Pair-Verify assumes an existing pairing; a synthesized
   RPIdentity may be refused even with correct keys (see `docs/m2-awdl-plan.md`
   § 4.1, § 5).
3. **4378 ABI drift.** The `awdl_*` iovar catalog and `interface_create` v3
   arguments were RE'd on 4364/4387; this firmware is a 2023 4378 build. Re-probe
   before trusting any offset (`~/Work/awdl-refs/`).
4. **It is unbuilt.** The patches apply cleanly but have never been compiled or
   loaded. Expect to iterate: fix build errors, sweep the create module params,
   re-probe the iovar surface, and adjust `awdl-up.sh` once you see what the
   firmware actually accepts.

## Failsafe note

`build.sh` deliberately does **not** install to `/lib/modules` (no DKMS, no
`depmod`), so a broken experiment never persists past a reboot. If you later
want the patched module to survive reboots *and* keep the stock module as an
automatic fallback, package it via DKMS to `updates/dkms/` (omdrop's model) —
but that is a hardening step for after this is proven to build and load, not for
the first attempt.

## Licence / provenance

Patches are GPL-2.0-only, derived from the Asahi kernel via
`brentkearney/omdrop-awdl`. `brcmiovar.py` (referenced by the scripts, default
`~/Work/awdl-refs/brcmiovar.py`) is from `andreanicassio/brcmfmac-awdl`.
