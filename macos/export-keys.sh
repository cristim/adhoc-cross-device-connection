#!/bin/bash
# export-keys.sh — key-export hygiene for ac-dc, under macOS.
#
# The export itself now lives in the Swift helper:
#
#     macos/ac-dc-send/.build/release/ac-dc export-keys
#
# It queries the keychain with SecItemCopyMatching and returns EVERY Continuity
# key, with SIP left enabled. This script used to carry its own `security
# find-generic-password` path, which returned only the FIRST item; on a machine
# with several devices that silently handed you one arbitrary key out of six, so
# it has been removed rather than left as a trap.
#
# What remains here is the part the Swift helper does not do:
#
#   * --arm-autowipe: install a one-shot LaunchAgent that deletes the exported
#     keys on your NEXT macOS login and then removes itself, so an export
#     survives exactly one Linux session.
#   * --wipe-now: delete the export immediately.
#   * --path-b: the Frida fallback, for when the keychain refuses to hand the
#     items to a normal process at all.
#
# Key handling / anti-leak design:
#   * Keys live only inside your FileVault-encrypted home, never in a synced
#     folder: ~/Library/Application Support/ac-dc, mode 700, file mode 600.
#   * That directory is excluded from Time Machine so no backup copy leaks.
#     (`ac-dc export-keys` applies the same exclusion.)
#   * Caveat: on an SSD with APFS copy-on-write plus wear-levelling, deletion
#     does NOT cryptographically erase the bytes. FileVault-at-rest is the real
#     protection; the auto-wipe is hygiene. Re-exporting is cheap, so we wipe
#     aggressively.

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

if command -v ac-dc-dump-to-keys >/dev/null 2>&1; then
    CONV="ac-dc-dump-to-keys"
else
    CONV="python3 \"$SCRIPT_DIR/dump-to-keys.py\""
fi

EXPORT_DIR="$HOME/Library/Application Support/ac-dc"
LABEL="app.ac-dc.cleanup"
AGENT_PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
CLEANUP_SH="$EXPORT_DIR/cleanup.sh"

write_cleanup_script() {
    mkdir -p "$EXPORT_DIR"
    chmod 700 "$EXPORT_DIR"
    tmutil addexclusion "$EXPORT_DIR" 2>/dev/null || true
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
}

# Install the agent WITHOUT launchctl-loading it, so it fires on the next login
# rather than wiping the file that was just written.
arm_autowipe() {
    write_cleanup_script
    [ -f "$EXPORT_DIR/keys.json" ] && chmod 600 "$EXPORT_DIR/keys.json"
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
    echo "Auto-wipe armed: the next macOS login deletes the export and removes the agent."
    echo "To wipe right now instead:  bash \"$0\" --wipe-now"
}

usage() {
    cat <<EOF
Usage: $(basename "$0") [--arm-autowipe | --wipe-now | --path-b]

Export the keys with the Swift helper (it returns every key; this script no
longer exports anything itself):

    cd "$SCRIPT_DIR/ac-dc-send" && swift build -c release
    ./.build/release/ac-dc export-keys

Then:
  --arm-autowipe   delete the export on the next macOS login (one-shot)
  --wipe-now       delete the export immediately
  --path-b         show the Frida fallback, for when the keychain refuses

Note that 'ac-dc send-key' can export and transfer in one step without ever
writing keys to disk, in which case there is nothing to wipe.
EOF
}

path_b() {
    cat <<EOF
== Path B: dump every key via Frida-hooking rapportd ==

Only needed if the keychain refuses to return the items to a normal process
(i.e. 'ac-dc export-keys' fails with a denial rather than a prompt). It uses
seemoo-lab's keychain_access tool, which hooks SecItemCopyMatching in rapportd.

  1. Disable SIP once (Recovery -> Terminal -> \`csrutil disable\`, reboot).
  2. pip3 install frida-tools    # or: brew install frida
  3. git clone https://github.com/seemoo-lab/apple-continuity-tools
  4. System Settings -> General -> AirDrop & Handoff: turn Handoff OFF.
  5. sudo python3 apple-continuity-tools/keychain_access/keychain_access.py rapportd \\
         -o "$EXPORT_DIR/dump.json"
  6. Turn Handoff back ON. rapportd reloads the keys; they print into dump.json.
  7. Ctrl-D to stop.

Convert the dump to keys.json in the secure dir (pure Python stdlib):

    $CONV \\
        "$EXPORT_DIR/dump.json" -o "$EXPORT_DIR/keys.json"
    chmod 600 "$EXPORT_DIR/keys.json"
    rm -f "$EXPORT_DIR/dump.json"          # the dump holds ALL keys in plaintext
    bash "$0" --arm-autowipe

Re-enable SIP afterwards (Recovery -> Terminal -> \`csrutil enable\`).
EOF
}

case "${1:-}" in
    # --arm-autowipe-only kept as an alias: older docs referenced it.
    --arm-autowipe|--arm-autowipe-only) arm_autowipe ;;
    --wipe-now)
        write_cleanup_script
        bash "$CLEANUP_SH"
        echo "Export wiped."
        ;;
    --path-b) path_b ;;
    -h|--help|"") usage ;;
    *) usage; exit 2 ;;
esac
