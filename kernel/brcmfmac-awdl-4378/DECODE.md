# Decoding a nearby Apple device's AWDL announcement

This is the workflow for the **discovery / control-plane decode** enabled by
patch `0005-brcmfmac-awdl-4378-event-forwarding.patch` plus the tools in
`tools/`. It lets you watch a nearby iPhone/Mac announce itself over AWDL — its
`<uuid>.local` hostname, AWDL version, announced services, and the master
election converging — read straight out of the frames the firmware already
receives.

> **This is discovery only.** It decodes what the firmware *receives* and lets
> you *announce* on the control plane. It does **not** transfer data: the AWDL
> data path is confirmed non-functional on this stack (brcmfmac gives us no
> monitor/injection and the firmware won't schedule addressed data for us). Do
> not expect AirDrop to complete. What works here is *seeing* the peer.

Everything below is **UNVERIFIED until you build and load** — patch 0005 is
written statically and has not been compiled or run. Where a step's outcome is
uncertain, it says so.

---

## 0. Prerequisites

- Apple M1 (BCM4378), Asahi kernel 7.1.13; `linux-asahi-headers` matching the
  running kernel installed.
- A **fallback network** (Ethernet/USB tether) ready: loading the module drops
  Wi-Fi for ~10 s, and while AWDL is enabled Wi-Fi throughput suffers.
- A second terminal with `sudo` ready, in case the link wedges.
- A target Apple device (iPhone/Mac) you can put into AirDrop **Everyone**.

## 1. Build the patched module (does not load anything)

```bash
cd kernel/brcmfmac-awdl-4378
./build.sh          # fetches asahi-7.1.13-2, applies 0001-0005, builds brcmfmac.ko
```

`build.sh` now applies `0005` after `0001-0004` (marker `brcmf_awdl_notify_fwevent`)
and only compiles `brcmfmac.ko`. It loads nothing; it prints the manual load
commands. The module is left at
`build/linux/drivers/net/wireless/broadcom/brcm80211/brcmfmac/brcmfmac.ko`.

## 2. Load it (deliberate — brief Wi-Fi drop)

```bash
sudo modprobe -r brcmfmac_wcc brcmfmac brcmutil
sudo modprobe brcmutil
sudo insmod build/linux/.../brcmfmac/brcmfmac.ko
sudo modprobe brcmfmac_wcc
```

Confirm Wi-Fi came back before continuing. To revert to the stock driver:
`sudo modprobe -r brcmfmac_wcc brcmfmac brcmutil && sudo modprobe brcmfmac`
(nothing was installed under `/lib/modules`, so a reboot also reverts).

## 3. Bring up awdl0

```bash
sudo ./awdl-up.sh          # creates awdl0 (vendor CREATE), configures + enables AWDL
```

Sanity checks (should already have worked before 0005):
```bash
ip link show awdl0                 # awdl0 present, UP
sudo dmesg | grep -iE 'awdl|E_IF' # BRCMF_E_IF_ADD / awdl attach lines
```

## 4. Start the event listener

The listener subscribes to the nl80211 `vendor` multicast group and prints the
firmware events patch 0005 forwards. Tee to a log so `awdlparse.py` can read it:

```bash
cd tools
sudo python3 awdlevents.py -v | tee events.log
```

Expected line on start:
`listening on nl80211 vendor group N (brcmfmac OUI 001018)`

Even before a peer is near, once AWDL is enabled you should see the firmware's
own periodic events: `AWDL_AW` (availability windows, ~40/s), `AWDL_ROLE`
(status 2 = master initially), and `ACTION_FRAME_COMPLETE` for the PSF/MIF it
transmits.

## 5. Make the Apple device announce

On the iPhone/Mac: open the AirDrop share sheet and set AirDrop to **Everyone**
(iOS: "Everyone for 10 Minutes"). Keep it within a metre or two.

Expected in the `awdlevents.py` stream:
- **`ACTION_FRAME_RX`** events sourced from the peer's AWDL MAC (its MIF/PSF),
  on channels 6/44/40.
- **`AWDL_ROLE`** transitioning **master (2) → slave (1)** as the firmware syncs
  to the peer as election master.
- `AWDL_PEER_STATE` / `AWDL_SYNC_STATE_CHANGED` around the same time.

> UNVERIFIED: whether the firmware raises `ACTION_FRAME_RX` (event 75) for these
> frames on the AWDL bsscfg, and where in the event payload the frame body sits.
> omdrop observed the events arriving but with the AWDL body **not** at
> `data + sizeof(brcmf_rx_mgmt_data)`; `awdlparse.py` copes by scanning the
> forwarded bytes for the `7f 00 17 f2` vendor-specific action signature. If
> `awdlparse.py` decodes nothing, dump a raw `ACTION_FRAME_RX` line from
> `events.log` and check where that signature actually falls.

## 6. Decode the peer's announcement

```bash
sudo python3 awdlparse.py events.log
```

Expected, per peer MAC + frame subtype (PSF/MIF):
```
== <peer-mac> MIF x37  (chanspec/rssi samples: ...)
   subtype            MIF
   awdl_version       10.0 iOS
   hostname           <uuid>.local
   sync               {...master, aw_period, ...}
   election           {...master, master_metric, ...}
   datapath_flags     0x9f23
   services           <hex...>
   tlvs: SYNC_PARAMS ELECTION_PARAMS ... ARPA VERSION ...
```

That `hostname` (ARPA TLV) is the peer's `<uuid>.local`, the `awdl_version` its
iOS/macOS AWDL version + device class, and `services` its announced service
responses (e.g. device name for `_applicationservicepairing`). Note: AirDrop's
`_airdrop._tcp` record is **not** in these frames — it is mDNS over the data
path, which does not work here.

## 7. (Optional) Announce ourselves to drive election convergence

If `AWDL_ROLE` is not converging (peer keeps us as our own master), publish our
host TLVs into the firmware's sync-frame template so we advertise a data-path
state, hostname and version:

```bash
sudo IF=wlan0 python3 announce.py           # or: announce.py <hostname>
```

Re-check `awdlevents.py`: the goal is a stable `AWDL_ROLE` slave with the peer
as master. (`announce.py` only affects the control plane; it does not make data
transfer work.)

## 8. Tear down

```bash
cd ..
sudo ./awdl-down.sh                          # disable AWDL, remove awdl0
# revert to stock driver when done:
sudo modprobe -r brcmfmac_wcc brcmfmac brcmutil && sudo modprobe brcmfmac
```

---

## What "success" means here

You have succeeded when `awdlevents.py` shows `ACTION_FRAME_RX` from the Apple
device and `AWDL_ROLE` converging to slave, and `awdlparse.py` prints the
device's `<uuid>.local` hostname and version/services. That is the full extent
of the goal: **decode the AWDL frames the firmware is already receiving.** The
data plane stays dead by design.
