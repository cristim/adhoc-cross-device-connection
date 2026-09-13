#!/usr/bin/env bash
# import-keys-from-macos.sh — pull handoff-clip's keys.json from the macOS
# install on this same machine, WITHOUT copying it onto any unencrypted disk
# beyond the destination you choose.
#
# It mounts the macOS APFS volume READ-ONLY (so it cannot alter macOS), reads
# the export that macos/export-keys.sh wrote inside FileVault, copies just
# keys.json out to the project dir with mode 600, and unmounts.
#
# UNTESTED against real hardware yet — device detection especially. Read the
# echoed steps before confirming. Read-only mounting cannot damage macOS.
#
# Requirements: apfs-fuse (AUR: apfs-fuse-git) or an apfs kernel module.
# FileVault: apfs-fuse prompts for the volume password (-p) when needed.

set -euo pipefail

REL="Library/Application Support/handoff-clip/keys.json"
REL_PLIST="Library/Application Support/handoff-clip/keys.plist"
DEST="${1:-$(cd "$(dirname "$0")/.." && pwd)/keys.json}"
MNT="$(mktemp -d /tmp/hc-macos.XXXXXX)"

cleanup() {
    if mountpoint -q "$MNT" 2>/dev/null || ls "$MNT" >/dev/null 2>&1 && [ -n "$(ls -A "$MNT" 2>/dev/null)" ]; then
        fusermount -u "$MNT" 2>/dev/null || umount "$MNT" 2>/dev/null || true
    fi
    rmdir "$MNT" 2>/dev/null || true
}
trap cleanup EXIT

if ! command -v apfs-fuse >/dev/null 2>&1; then
    echo "apfs-fuse not found. Install it (Arch/Asahi):  yay -S apfs-fuse-git" >&2
    echo "Alternatively load an apfs kernel module and mount -o ro yourself." >&2
    exit 1
fi

echo "APFS-type partitions on this machine:"
lsblk -o NAME,SIZE,FSTYPE,LABEL,PARTTYPENAME 2>/dev/null | grep -iE 'apple|apfs|NAME' || \
    lsblk -o NAME,SIZE,FSTYPE 2>/dev/null
echo
echo "The macOS 'Data' volume (the one holding /Users) is what you want."
read -r -p "APFS container partition device (e.g. /dev/nvme0n1p2): " DEV
[ -b "$DEV" ] || { echo "Not a block device: $DEV" >&2; exit 1; }

# An APFS *container* can hold several volumes (System, Data, Preboot, ...).
# apfs-fuse mounts one volume; the Data volume is usually index 1 (0-based) but
# varies. List them first so the user can pick.
echo
echo "Volumes in $DEV:"
apfs-fuse -l "$DEV" 2>/dev/null || echo "(could not list; will try volume 0)"
read -r -p "Volume index holding /Users [default 1]: " VOL
VOL="${VOL:-1}"

echo "Mounting $DEV volume $VOL read-only at $MNT (FileVault password if prompted)..."
# -o ro : read only. -s <n> : volume index. apfs-fuse asks for the password.
apfs-fuse -o ro -s "$VOL" "$DEV" "$MNT"

# Find the user home that has the export.
SRC=""
for home in "$MNT"/Users/*; do
    if [ -f "$home/$REL" ]; then SRC="$home/$REL"; break; fi
    if [ -f "$home/$REL_PLIST" ]; then SRC="$home/$REL_PLIST"; DEST="${DEST%.json}.plist"; break; fi
done

if [ -z "$SRC" ]; then
    echo "No handoff-clip export found under any /Users home on this volume." >&2
    echo "Did you run macos/export-keys.sh in macOS? Is this the Data volume?" >&2
    exit 1
fi

install -m 600 "$SRC" "$DEST"
echo "Imported: $DEST (mode 600)"
echo "Run:  handoff-clip scan --keys \"$DEST\""
echo
echo "Reminder: the macOS copy auto-wipes on your next macOS login. This local"
echo "copy persists — it lives on your Linux disk; delete it when done if that"
echo "partition is not itself encrypted."
