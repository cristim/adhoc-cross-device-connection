#!/usr/bin/env bash
#
# build.sh - build a patched brcmfmac.ko (AWDL/awdl0 for BCM4378) out-of-tree.
#
# THIS SCRIPT ONLY BUILDS. It never loads, unloads, installs, or touches the
# running module or /lib/modules. It fetches the matching Asahi kernel source,
# applies the AWDL patches, compiles ONLY brcmfmac.ko against the *running*
# kernel's build headers (so the vermagic matches and it can be insmod'd), and
# then prints the manual load / unload / revert commands for you to run
# deliberately, later, with a fallback network and recovery ready.
#
# Nothing here risks your Wi-Fi. Loading the module (which does carry risk) is a
# separate, manual step you perform yourself - see the instructions this prints,
# and README.md.
#
set -euo pipefail

# --- configuration (override via environment) --------------------------------
readonly KVER="$(uname -r)"                     # e.g. 7.1.13-1-1-ARCH (running)
readonly KDIR="/lib/modules/${KVER}/build"      # installed kernel build headers
readonly ASAHI_TAG="${ASAHI_TAG:-asahi-7.1.13-2}"   # matches linux-asahi 7.1.13.asahi2-1
readonly ASAHI_REPO="${ASAHI_REPO:-https://github.com/AsahiLinux/linux.git}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly HERE="$here"
# Where to put / find the kernel source tree. Override KSRC to reuse an existing
# checkout and skip the (large) clone.
readonly WORK="${WORK:-${HERE}/build}"
readonly KSRC="${KSRC:-${WORK}/linux}"
readonly MODSUBDIR="drivers/net/wireless/broadcom/brcm80211/brcmfmac"
readonly MODDIR="${KSRC}/${MODSUBDIR}"

log()  { printf '[build] %s\n' "$*" >&2; }
die()  { printf '[build] ERROR: %s\n' "$*" >&2; exit 1; }

# --- sanity checks -----------------------------------------------------------
[[ -d "$KDIR" ]] || die "kernel build headers not found at $KDIR
    Install them:  sudo pacman -S linux-asahi-headers
    (must match the RUNNING kernel: $KVER)"

command -v git   >/dev/null || die "git not found (needed to fetch the Asahi source)"
command -v make  >/dev/null || die "make not found (pacman -S base-devel)"
command -v gcc   >/dev/null || die "gcc not found (pacman -S base-devel)"
command -v patch >/dev/null || die "patch not found (pacman -S patch)"

log "running kernel : $KVER"
log "kernel headers : $KDIR"
log "asahi tag      : $ASAHI_TAG"
log "kernel source  : $KSRC"

# The patches were written and verified against the Asahi driver tree at
# $ASAHI_TAG, which corresponds to linux-asahi 7.1.13.asahi2-1. The running
# kernel here is 7.1.13-1-1-ARCH; the brcmfmac source is identical across that
# pkgrel bump, and we compile against the running kernel's own headers so the
# resulting .ko carries the running kernel's vermagic.

# --- fetch kernel source (shallow) -------------------------------------------
if [[ ! -d "$KSRC/.git" ]]; then
    log "cloning Asahi kernel at $ASAHI_TAG (shallow; this downloads a large tree)..."
    mkdir -p "$WORK"
    git clone --depth 1 --branch "$ASAHI_TAG" "$ASAHI_REPO" "$KSRC"
else
    log "reusing existing kernel source at $KSRC"
    log "  (checked-out ref: $(git -C "$KSRC" describe --tags --always 2>/dev/null || echo unknown))"
fi

[[ -d "$MODDIR" ]] || die "brcmfmac source dir missing: $MODDIR"

# --- apply the AWDL patches --------------------------------------------------
# Idempotent: skip a patch that is already applied (marker symbol present).
apply_patch() {
    local pfile="$1" marker="$2"
    if grep -rq "$marker" "$MODDIR"/*.c "$MODDIR"/*.h 2>/dev/null; then
        log "already applied: $(basename "$pfile")  (found '$marker')"
        return 0
    fi
    log "applying: $(basename "$pfile")"
    # -p1 from the kernel source root. --forward makes a re-run a no-op instead
    # of prompting; if it does not apply cleanly we stop rather than force.
    patch -p1 -d "$KSRC" --forward --no-backup-if-mismatch -i "$pfile" \
        || die "patch failed: $(basename "$pfile") - the tree may not match $ASAHI_TAG"
}

apply_patch "${HERE}/0001-brcmfmac-awdl-4378-interface-create.patch"     "brcmf_awdl_add_vif"
apply_patch "${HERE}/0002-brcmfmac-awdl-4378-netdev-ops.patch"           "brcmf_netdev_open_awdl"
apply_patch "${HERE}/0003-brcmfmac-awdl-4378-usable-create-args.patch"   "awdl_create_flags"
apply_patch "${HERE}/0004-brcmfmac-awdl-4378-rtnl-safe-teardown.patch"   "interface_remove failed"

# --- build ONLY brcmfmac.ko --------------------------------------------------
# M= points make at the module subdir; -C at the running kernel's build headers.
# Only brcmfmac.ko is rebuilt; brcmutil/brcmfmac_wcc/cfg80211 stay stock and
# their symbols resolve from $KDIR/Module.symvers.
log "building brcmfmac.ko (this compiles only the one module)..."
make -C "$KDIR" M="$MODDIR" modules

KO="${MODDIR}/brcmfmac.ko"
[[ -f "$KO" ]] || die "build reported success but $KO is missing"

log "build OK"

# --- report + MANUAL next steps (NOT executed) -------------------------------
cat >&2 <<EOF

================================================================================
 BUILD COMPLETE - nothing has been loaded. The patched module is at:

   $KO

 vermagic (must match the running kernel to load):
   $(modinfo -F vermagic "$KO" 2>/dev/null || echo '  (install kmod to read vermagic)')
   running kernel: $KVER

 AWDL create tunables are module parameters (sweepable without rebuilding):
   awdl_create_flags (default 0x1a)   awdl_if_index (default 2)
   awdl_bssid (default 00:25:00:ff:94:73)
 These were tuned on BCM4387 and are UNVERIFIED on this BCM4378 - see README.md.

--------------------------------------------------------------------------------
 TO LOAD (do this DELIBERATELY, later - it briefly drops Wi-Fi and carries the
 risks in README.md; have a FALLBACK NETWORK up and a second sudo terminal open):

   # 1. unload the stock stack (~10s Wi-Fi drop)
   sudo modprobe -r brcmfmac_wcc brcmfmac brcmutil
   # 2. load the patched brcmfmac plus its stock companions
   sudo modprobe brcmutil
   sudo insmod "$KO"
   sudo modprobe brcmfmac_wcc
   # 3. confirm Wi-Fi came back, THEN bring up AWDL:
   #    sudo ./awdl-up.sh     (creates awdl0 via the new vendor command)

 TO UNLOAD / REVERT TO THE STOCK DRIVER (no trace left; a reboot also reverts,
 because nothing was installed under /lib/modules):

   sudo modprobe -r brcmfmac_wcc brcmfmac brcmutil
   sudo modprobe brcmfmac        # loads the stock in-tree module + wcc + brcmutil

 If Wi-Fi is wedged and commands hang, reboot - stock loads on next boot.
================================================================================
EOF
