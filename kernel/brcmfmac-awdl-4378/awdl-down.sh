#!/usr/bin/env bash
#
# awdl-down.sh - disable AWDL and destroy awdl0 on the built-in BCM4378.
#
# REQUIRES THE PATCHED brcmfmac (same as awdl-up.sh). Safe to run any time to
# return the radio's attention fully to wlan0. Needs root.
#
# It disables AWDL (awdl 0) and then removes the interface via the
# BRCMF_VNDR_CMDS_AWDL *DESTROY* subcommand (subcmd 2, op 1). Teardown is
# asynchronous in the driver (the netdev goes away when the firmware's
# BRCMF_E_IF_DEL event is processed) - so we poll for awdl0 to disappear.
#
set -euo pipefail

readonly IF="${IF:-wlan0}"
readonly AWDL_IF="${AWDL_IF:-awdl0}"
readonly BROADCOM_OUI="0x001018"
readonly SUBCMD_AWDL="2"
readonly BRCMIOVAR="${BRCMIOVAR:-$HOME/Work/awdl-refs/brcmiovar.py}"

log() { printf '[awdl-down] %s\n' "$*" >&2; }
die() { printf '[awdl-down] ERROR: %s\n' "$*" >&2; exit 1; }

[[ "$(id -u)" -eq 0 ]] || die "must run as root (needs CAP_NET_ADMIN)"
command -v iw >/dev/null || die "iw not found"

# 1. disable AWDL first (best-effort; ignore errors so teardown still proceeds)
if ip link show "$AWDL_IF" >/dev/null 2>&1 && [[ -r "$BRCMIOVAR" ]]; then
    log "disabling AWDL (awdl 0)..."
    python3 "$BRCMIOVAR" -i "$AWDL_IF" setint awdl 0 >/dev/null 2>&1 || log "awdl 0 returned an error (continuing)"
    ip link set "$AWDL_IF" down 2>/dev/null || true
fi

# 2. destroy the interface via the vendor DESTROY subcommand (op = 1)
if ip link show "$AWDL_IF" >/dev/null 2>&1; then
    log "destroying $AWDL_IF via vendor DESTROY (op 1)..."
    iw dev "$IF" vendor send "$BROADCOM_OUI" "$SUBCMD_AWDL" 0x01 0x00 0x00 0x00 \
        || log "vendor DESTROY returned an error (continuing)"
    for _ in $(seq 1 25); do
        ip link show "$AWDL_IF" >/dev/null 2>&1 || break
        sleep 0.2
    done
fi

if ip link show "$AWDL_IF" >/dev/null 2>&1; then
    log "WARNING: $AWDL_IF still present. AWDL is disabled, but the netdev did not"
    log "         go away. If Wi-Fi is fine you can leave it; to force-remove,"
    log "         reload the driver:  sudo modprobe -r brcmfmac_wcc brcmfmac brcmutil"
    log "         then reload the patched or stock module (see README.md)."
else
    log "$AWDL_IF removed; AWDL is off."
fi
