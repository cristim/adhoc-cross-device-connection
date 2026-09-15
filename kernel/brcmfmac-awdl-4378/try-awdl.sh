#!/usr/bin/env bash
#
# try-awdl.sh - one-shot: load the patched brcmfmac, verify Wi-Fi survives,
# bring up AWDL (awdl0), and dump diagnostics. Run as root:
#
#     sudo /home/cristi/Work/apple-cross-device-clipboard/kernel/brcmfmac-awdl-4378/try-awdl.sh
#
# SAFETY:
#  * Have a fallback link up (you do: iPhone USB tether). Loading briefly drops wlan0.
#  * If wlan0 does NOT come back after loading the patched module, this script
#    AUTO-REVERTS to the stock driver (so normal Wi-Fi is restored) and stops.
#  * If AWDL bring-up fails but wlan0 is fine, it LEAVES the patched driver loaded
#    and just reports - so we can iterate. Revert manually with the printed command
#    (or reboot: nothing is installed under /lib/modules, so boot restores stock).
#
# Not set -e: we want to continue through non-fatal errors and always reach the
# diagnostics.
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
KO="$HERE/build/linux/drivers/net/wireless/broadcom/brcm80211/brcmfmac/brcmfmac.ko"

# Resolve the invoking user's home even under sudo ($HOME would be /root).
if [[ -n "${SUDO_USER:-}" ]]; then
    REAL_HOME="$(getent passwd "$SUDO_USER" | cut -d: -f6)"
else
    REAL_HOME="$HOME"
fi
BRCMIOVAR="${BRCMIOVAR:-$REAL_HOME/Work/awdl-refs/brcmiovar.py}"

log()  { printf '\n== %s\n' "$*"; }
info() { printf '   %s\n' "$*"; }

[[ "$(id -u)" -eq 0 ]] || { echo "must run as root:  sudo $0" >&2; exit 1; }
[[ -f "$KO" ]]          || { echo "patched module not found: $KO (run build.sh first)" >&2; exit 1; }
[[ -r "$BRCMIOVAR" ]]   || { echo "brcmiovar.py not found: $BRCMIOVAR" >&2; exit 1; }
command -v iw >/dev/null || { echo "iw not installed (pacman -S iw)" >&2; exit 1; }

dmesg_tail() { dmesg -T 2>/dev/null | grep -iE 'brcmfmac|awdl|E_IF|ieee80211' | tail -"${1:-60}"; }

revert_to_stock() {
    log "REVERTING to the stock driver"
    modprobe -r brcmfmac_wcc brcmfmac brcmutil 2>/dev/null
    rmmod brcmfmac 2>/dev/null
    modprobe brcmfmac 2>/dev/null
    sleep 2
    if ip link show wlan0 >/dev/null 2>&1; then info "stock driver reloaded, wlan0 present"
    else info "wlan0 still missing - reboot to fully restore (nothing is installed under /lib/modules)"; fi
}

log "interfaces before"
ip -brief link

# --- 1. swap in the patched module ------------------------------------------
log "unloading stock brcmfmac stack (wlan0 drops ~10s; USB tether stays up)"
modprobe -r brcmfmac_wcc brcmfmac brcmutil 2>&1 | sed 's/^/   /' || true
rmmod brcmfmac 2>/dev/null || true

log "loading PATCHED brcmfmac"
modprobe brcmutil 2>&1 | sed 's/^/   /' || true
if ! insmod "$KO" 2>&1 | sed 's/^/   /'; then
    info "insmod FAILED"
    dmesg_tail 40
    revert_to_stock
    exit 1
fi
modprobe brcmfmac_wcc 2>&1 | sed 's/^/   /' || true

# --- 2. did normal Wi-Fi survive? -------------------------------------------
log "waiting up to 20s for wlan0 to come back"
for _ in $(seq 1 40); do ip link show wlan0 >/dev/null 2>&1 && break; sleep 0.5; done
if ! ip link show wlan0 >/dev/null 2>&1; then
    info "wlan0 did NOT return - the patched module broke normal operation"
    dmesg_tail 60
    revert_to_stock
    exit 1
fi
info "wlan0 is back"

# Confirm it is OUR module (the AWDL params only exist on the patched build).
if [[ -e /sys/module/brcmfmac/parameters/awdl_create_flags ]]; then
    info "patched module confirmed (awdl_* module params present)"
else
    info "WARNING: awdl_create_flags param missing - stock module may be loaded, not the patch"
fi

# --- 3. bring up AWDL (experimental; do not abort on failure) ----------------
log "bringing up AWDL via awdl-up.sh"
BRCMIOVAR="$BRCMIOVAR" IF=wlan0 bash "$HERE/awdl-up.sh" 2>&1 | sed 's/^/   /' || info "awdl-up.sh returned non-zero (see dmesg below)"

# --- 4. report ---------------------------------------------------------------
log "awdl0 status"
ip -brief link show awdl0 2>&1 | sed 's/^/   /' || info "awdl0 NOT present"
log "recent kernel log (brcmfmac / awdl)"
dmesg_tail 80

cat <<EOF

== done. To revert to the stock Wi-Fi driver at any time:
   sudo modprobe -r brcmfmac_wcc brcmfmac brcmutil && sudo modprobe brcmfmac
   (or just reboot - stock loads on next boot)
EOF
