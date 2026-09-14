#!/usr/bin/env bash
#
# awdl-up.sh - create and enable AWDL (awdl0) on the built-in BCM4378.
#
# REQUIRES THE PATCHED brcmfmac FROM THIS DIRECTORY TO BE LOADED (build.sh, then
# the manual load steps it prints). Against the stock module there is no awdl0
# vendor command and this script does nothing useful.
#
# It creates awdl0 via the new BRCMF_VNDR_CMDS_AWDL *CREATE* vendor subcommand
# (subcmd 2, op 0) - NOT the awdl_if iovar, which is UNSUPPORTED on this 4378
# firmware - then configures the firmware's AWDL engine (sync params, channel
# sequence, config) the way Apple's driver does and enables it.
#
# WARNING: while AWDL is on, the radio time-shares off the AP channel, so Wi-Fi
# throughput drops and, with a bad channel sequence, the link can become
# unusable. Have a FALLBACK NETWORK up. Run awdl-down.sh when done. Needs root.
#
set -euo pipefail

readonly IF="${IF:-wlan0}"                  # primary (infra) interface
readonly AWDL_IF="${AWDL_IF:-awdl0}"
readonly BROADCOM_OUI="0x001018"
readonly SUBCMD_AWDL="2"                     # BRCMF_VNDR_CMDS_AWDL
readonly MASTER_CHAN="${MASTER_CHAN:-6}"
readonly BRCMIOVAR="${BRCMIOVAR:-$HOME/Work/awdl-refs/brcmiovar.py}"

log() { printf '[awdl-up] %s\n' "$*" >&2; }
die() { printf '[awdl-up] ERROR: %s\n' "$*" >&2; exit 1; }

[[ "$(id -u)" -eq 0 ]] || die "must run as root (needs CAP_NET_ADMIN)"
command -v iw >/dev/null || die "iw not found"
[[ -r "$BRCMIOVAR" ]] || die "brcmiovar.py not found at $BRCMIOVAR (set BRCMIOVAR=...)"
ip link show "$IF" >/dev/null 2>&1 || die "primary interface $IF not present"

iov() { python3 "$BRCMIOVAR" "$@"; }

# --- 1. create awdl0 via the vendor CREATE subcommand ------------------------
# Creation is ASYNCHRONOUS in the patched driver: the vendor command only asks
# firmware; the netdev is registered later by the fweh worker. So we issue
# CREATE and then POLL for awdl0 to appear - never assume it exists on return.
if ! ip link show "$AWDL_IF" >/dev/null 2>&1; then
    log "creating $AWDL_IF via vendor CREATE (oui $BROADCOM_OUI subcmd $SUBCMD_AWDL op 0)..."
    # op = 0 (CREATE) as a u32 little-endian: 4 bytes 00 00 00 00
    iw dev "$IF" vendor send "$BROADCOM_OUI" "$SUBCMD_AWDL" 0x00 0x00 0x00 0x00 \
        || die "vendor CREATE failed - is the PATCHED brcmfmac loaded?"
    for _ in $(seq 1 25); do
        ip link show "$AWDL_IF" >/dev/null 2>&1 && break
        sleep 0.2
    done
    ip link show "$AWDL_IF" >/dev/null 2>&1 \
        || die "$AWDL_IF did not appear (firmware may not have emitted BRCMF_E_IF_ADD;
    check: sudo dmesg | grep -iE 'brcmfmac|awdl|E_IF')"
    log "$AWDL_IF created"
else
    log "$AWDL_IF already exists, reusing"
fi

ip link set "$AWDL_IF" up

# From here iovars are issued ON the awdl0 netdev, which maps to the AWDL bsscfg
# the firmware created - so no explicit "bsscfg:" scoping is needed.

# --- 2. sync params (awdl_sync_params_t, 36 B) -------------------------------
# Mirrors what a nearby iOS device advertises: master channel, aw_period 16 TU,
# action-frame period ~110 TU (firmware default is 1000 TU = ~9x rarer than
# Apple), ext counts 3/3/3/3, presence mode 4.
SYNC="$(MC="$MASTER_CHAN" AF="${AF_PERIOD:-110}" python3 -c '
import os, struct
b = bytearray(36)
b[6] = int(os.environ["MC"])                                   # master_chan
b[7] = 0                                                        # guard_time
struct.pack_into("<HHH", b, 8, 16, int(os.environ["AF"]), 0)   # aw_period, af_period, flags
struct.pack_into("<HHH", b, 14, 16, 16, 0)                     # aw_ext_len, aw_cmn_len, aw_remaining
b[20:24] = bytes([3, 3, 3, 3])                                  # min/max ext counts
b[30] = 4                                                       # presence_mode
print(b.hex())')"
iov -i "$AWDL_IF" set awdl_sync_params "$SYNC" >/dev/null

# --- 3. channel sequence (enc=2 D11AC chanspecs, 16 slots) -------------------
# SPARSE, Apple-style: infra channel (= the AP's, so wlan0 keeps working) in
# most slots, 5 GHz social ch in slots 2/10, 2.4 GHz social ch in slot 8. A
# dense all-social sequence makes Wi-Fi unusable - do NOT do that.
INFRA_CHAN="${INFRA_CHAN:-$(iw dev "$IF" link 2>/dev/null | awk '/freq:/{f=$2} END{if(f>5000)print int((f-5000)/5); else if(f)print int((f-2407)/5)}')}"
[[ -z "$INFRA_CHAN" ]] && INFRA_CHAN=44
SEQ="$(INFRA="$INFRA_CHAN" S5="${SOCIAL5:-44}" S2="${SOCIAL2:-6}" python3 -c '
import os, struct
inf=int(os.environ["INFRA"]); s5=int(os.environ["S5"]); s2=int(os.environ["S2"])
cs=lambda c:(0xC000 if c>14 else 0)|0x1000|c
slots=[inf]*16
slots[2]=s5; slots[8]=s2; slots[10]=s5
print((bytes([15,2,0,3])+b"\xff\xff"+b"".join(struct.pack(">H",cs(c)) for c in slots)).hex())')"
iov -i "$AWDL_IF" set awdl_chan_seq "$SEQ" >/dev/null

iov -i "$AWDL_IF" set awdl_extcounts 03030303 >/dev/null   || log "awdl_extcounts rejected (non-fatal)"
iov -i "$AWDL_IF" setint awdl_presencemode 4 >/dev/null    || log "awdl_presencemode rejected (non-fatal)"
iov -i "$AWDL_IF" setint awdl_aftxmode 0 >/dev/null        || log "awdl_aftxmode rejected (non-fatal)"

# --- 4. config MUST precede enable (awdl 1 returns BADOPTION otherwise) -------
iov -i "$AWDL_IF" setint awdl_config 115 >/dev/null        # value Apple's driver uses
iov -i "$AWDL_IF" setint awdl_af_rssi -90 >/dev/null || true

# --- 5. enable -------------------------------------------------------------
# Leave the election self-metric at 0: writing any non-zero metric makes this
# firmware elect itself master and desync from the peer.
iov -i "$AWDL_IF" setint awdl 1 >/dev/null

STATE="$(iov -i "$AWDL_IF" getint awdl 2>/dev/null || echo '?')"
log "AWDL enabled on $AWDL_IF (master channel $MASTER_CHAN); awdl=$STATE"
log "verify:  ip link show $AWDL_IF   &&   ac-dc discover"
log "NOTE: these firmware iovar names/sizes are UNVERIFIED on 4378 - if a step"
log "      errors, re-probe with brcmiovar.py + iovars-awdl.txt and adjust."
