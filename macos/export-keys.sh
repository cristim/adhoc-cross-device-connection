#!/bin/bash
# export-keys.sh — run this ONCE under macOS (booted natively, signed into the
# same Apple ID as your iPhone) to export the Continuity BLE encryption keys
# that ac-dc needs on the Linux side.
#
# Key handling / anti-leak design:
#   * Keys are written ONLY inside your FileVault-encrypted home, never to a
#     synced or shared folder, so they are encrypted at rest.
#       - target dir: ~/Library/Application Support/ac-dc
#       - NOT ~/Desktop or ~/Documents (iCloud-synced when Desktop&Documents on)
#   * The dir is excluded from Time Machine (tmutil) so no backup copy leaks.
#   * File mode is 600 (owner-only).
#   * A one-shot LaunchAgent is installed that, on your NEXT macOS login,
#     deletes the exported keys and then removes itself. So the export survives
#     exactly one Linux session and macOS wipes it automatically afterwards.
#   * Caveat: on an SSD with APFS copy-on-write + wear-leveling, deletion does
#     NOT cryptographically erase the bytes. FileVault-at-rest is the actual
#     protection; the auto-wipe is hygiene, not a secure erase. Re-exporting is
#     cheap (keys are long-term + iCloud-synced), so we wipe aggressively.
#
# On Linux/Asahi, pull the file with scripts/import-keys-from-macos.sh, which
# mounts this volume READ-ONLY so the key never leaves encrypted storage.
#
# Two extraction paths below. Try A first; if it comes up empty, use B.

set -euo pipefail

# Resolve our real directory even when invoked via a symlink (e.g. a Homebrew
# bin shim that points into libexec), so we can locate dump-to-keys.py.
SOURCE="${BASH_SOURCE[0]}"
while [ -h "$SOURCE" ]; do
    DIR="$(cd -P "$(dirname "$SOURCE")" >/dev/null 2>&1 && pwd)"
    SOURCE="$(readlink "$SOURCE")"
    [[ $SOURCE != /* ]] && SOURCE="$DIR/$SOURCE"
done
SCRIPT_DIR="$(cd -P "$(dirname "$SOURCE")" >/dev/null 2>&1 && pwd)"

# Prefer the Homebrew-installed converter command if it's on PATH, else the
# sibling script next to us (repo checkout).
if command -v ac-dc-dump-to-keys >/dev/null 2>&1; then
    CONV="ac-dc-dump-to-keys"
else
    CONV="python3 \"$SCRIPT_DIR/dump-to-keys.py\""
fi

EXPORT_DIR="$HOME/Library/Application Support/ac-dc"
LABEL="app.ac-dc.cleanup"
AGENT_PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
CLEANUP_SH="$EXPORT_DIR/cleanup.sh"

mkdir -p "$EXPORT_DIR"
chmod 700 "$EXPORT_DIR"
# Keep Time Machine from backing up the keys.
tmutil addexclusion "$EXPORT_DIR" 2>/dev/null || true

# install_autowipe: drop a one-shot LaunchAgent that fires on the NEXT login
# (we do NOT launchctl-load it now, so it won't wipe the file we just wrote).
install_autowipe() {
    cat > "$CLEANUP_SH" <<CLEAN
#!/bin/bash
# One-shot: erase the ac-dc key export, then remove self.
/bin/rm -f "$EXPORT_DIR/keys.json" "$EXPORT_DIR/keys.plist" "$EXPORT_DIR/dump.json" 2>/dev/null
/bin/launchctl bootout "gui/\$(id -u)/$LABEL" 2>/dev/null || true
/bin/rm -f "$AGENT_PLIST"
/bin/rm -f "$CLEANUP_SH"
/bin/rmdir "$EXPORT_DIR" 2>/dev/null || true
CLEAN
    chmod 700 "$CLEANUP_SH"

    mkdir -p "$HOME/Library/LaunchAgents"
    cat > "$AGENT_PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>$LABEL</string>
    <key>ProgramArguments</key>
    <array>
        <string>/bin/bash</string>
        <string>$CLEANUP_SH</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>
PLIST
    echo
    echo "Auto-wipe armed: next macOS login will delete the export and remove the agent."
    echo "  (to wipe right now instead:  bash \"$CLEANUP_SH\" )"
}

secure_finish() {
    # $1 = path just written
    chmod 600 "$1"
    install_autowipe
    echo
    echo "Exported to: $1"
    echo "On Linux:    ./scripts/import-keys-from-macos.sh   (mounts this volume read-only)"
}

# Called as `export-keys.sh --arm-autowipe-only` after a manual Path B
# conversion: just arm the auto-wipe for the file already in place, then stop.
if [ "${1:-}" = "--arm-autowipe-only" ]; then
    [ -f "$EXPORT_DIR/keys.json" ] && chmod 600 "$EXPORT_DIR/keys.json"
    install_autowipe
    echo "Armed auto-wipe for $EXPORT_DIR/keys.json"
    exit 0
fi

echo "== Path A: security CLI (works if the items are readable in your login keychain)"
# The Continuity keys are generic-password items under this service. They are
# synchronizable + protected, so the plain security CLI may or may not return
# them depending on macOS version and ACL. We ask for the raw data blob (a
# binary plist) and let the Rust side parse it.
if security find-generic-password -s "com.apple.continuity.encryption" -w >/tmp/hc_key.hex 2>/dev/null; then
    xxd -r -p /tmp/hc_key.hex > "$EXPORT_DIR/keys.plist"
    rm -f /tmp/hc_key.hex
    echo "Exported one raw keychain item."
    echo "NOTE: security CLI returns only ONE item. If you have multiple devices,"
    echo "      use Path B to capture all keys."
    echo "ac-dc parses this plist directly (pass it as --keys)."
    secure_finish "$EXPORT_DIR/keys.plist"
    exit 0
fi

cat <<EOF
Path A returned nothing (expected on recent macOS — the item ACL blocks the
plain security CLI).

== Path B: dump every key via Frida-hooking rapportd (reliable) ==

This uses seemoo-lab's keychain_access tool, which hooks SecItemCopyMatching in
rapportd and prints every Continuity key for all your iCloud devices.

  1. Disable SIP once (Recovery -> Terminal -> \`csrutil disable\`, reboot).
  2. pip3 install frida-tools    # or: brew install frida
  3. git clone https://github.com/seemoo-lab/apple-continuity-tools
  4. In System Settings -> General -> AirDrop & Handoff, turn Handoff OFF.
  5. sudo python3 apple-continuity-tools/keychain_access/keychain_access.py rapportd \\
         -o "$EXPORT_DIR/dump.json"
  6. Turn Handoff back ON. rapportd reloads the keys; they print into dump.json.
  7. Ctrl-D to stop.

Then convert the dump to keys.json IN THE SECURE DIR (pure Python stdlib):

    $CONV \\
        "$EXPORT_DIR/dump.json" -o "$EXPORT_DIR/keys.json"
    chmod 600 "$EXPORT_DIR/keys.json"
    rm -f "$EXPORT_DIR/dump.json"          # the dump holds ALL keys in plaintext
    bash "$SCRIPT_DIR/export-keys.sh" --arm-autowipe-only

  dump-to-keys.py walks the whole dump, decodes every hex/base64/plist blob it
  finds, and keeps the ones that parse to a keychain item carrying \`keyData\`
  (service "com.apple.continuity.encryption"). It skips wrapped keys (not
  directly usable) and unrelated items automatically.

Then on Linux:  ./scripts/import-keys-from-macos.sh
EOF
exit 1
