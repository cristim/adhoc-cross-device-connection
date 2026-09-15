# AWDL decode tooling (userspace)

Userspace helpers for the AWDL **discovery / control plane** on the patched
brcmfmac (BCM4378). They read the firmware events that patch
`0005-brcmfmac-awdl-4378-event-forwarding.patch` forwards to userspace, and
decode the PSF/MIF action frames a nearby Apple device transmits.

**Discovery only.** These tools observe and (optionally) announce on the AWDL
control plane. They do **not** move data — the AWDL data path is confirmed
non-functional on this stack (see `../../docs/` and the lab notes).

## Files

| file | what it does |
|---|---|
| `awdlevents.py` | Subscribe to the nl80211 `vendor` multicast group and print a decoded stream of the forwarded firmware events: AW windows (96), ROLE master/slave (97), AWDL_EVENT subtypes (98), and ACTION_FRAME_RX (75). Writes lines that `awdlparse.py` reads. |
| `awdlparse.py` | Decode the PSF/MIF action-frame TLVs from an `awdlevents.py` log into a peer's `<uuid>.local` hostname (ARPA TLV 16), AWDL version + device class (21), sync (4) / election (5, 24) parameters, data-path state (12) and announced services (2, 6). |
| `announce.py` | Publish our own host TLVs (data-path state + ARPA hostname + version) into the firmware's sync-frame template via the `awdl_payload` iovar, so the election can converge (needed before a peer will treat us as a synced neighbour). |
| `brcmiovar.py` | Shared low-level helper: the raw brcmfmac dcmd/iovar channel over the nl80211 vendor command, and the genetlink plumbing the other scripts import. |

## Provenance & licensing

**These four scripts are brought in from
[`andreanicassio/brcmfmac-awdl`](https://github.com/andreanicassio/brcmfmac-awdl)
and are licensed ISC** (Copyright (c) 2026 Andrea Nicassio — see the repo's
`LICENSE`). They are shipped essentially verbatim; the only adaptation is
`announce.py`'s default interface (`wlan0` on this M1 4378 box, overridable with
the `IF` env var) and bsscfg index, to match `../awdl-up.sh` / `../build.sh`.

That repository, and the kernel-side forwarding it pairs with, build on
[`brentkearney/omdrop-awdl`](https://github.com/brentkearney/omdrop-awdl)
(GPL-2.0-only), whose patches 0009–0011 first instrumented and forwarded the
AWDL action frames.

The **kernel patch** `0005-...-event-forwarding.patch` is a derivative of the
Linux `brcmfmac` driver and is therefore **GPL-2.0-only**, kept clearly separate
from the MIT-licensed Rust crate at the repository root.

## Requirements

Python 3, root (CAP_NET_ADMIN), and the patched module loaded with `awdl0`
brought up (`../build.sh` → load → `../awdl-up.sh`). No third-party Python
packages — `brcmiovar.py` speaks genetlink by hand.

See `../DECODE.md` for the full workflow.
