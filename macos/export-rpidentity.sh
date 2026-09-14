#!/bin/bash
# export-rpidentity.sh — run this ONCE under macOS (booted natively, signed into
# the same Apple ID as your other devices) to produce the RPIdentity long-term
# Ed25519 identity that companion-link Pair-Verify needs on the Linux side.
#
# This is the SIBLING of export-keys.sh (which exports only the BLE Continuity
# AES keys, service com.apple.continuity.encryption). Pair-Verify additionally
# needs the `RPIdentity-SameAccountDevice` identity — see macos/RPIDENTITY.md and
# ../src/companion.rs (PairingIdentity::load).
#
# APPROACH (b) — RECOMMENDED (see RPIDENTITY.md for why (a) is a dead end):
#   1. GENERATE our own Ed25519 keypair (inject-rpidentity.swift / CryptoKit).
#   2. INJECT its PUBLIC key into the iCloud keychain as a new
#      RPIdentity-SameAccountDevice item, so all same-account devices trust us.
#   3. Keep our PRIVATE seed locally, and read every peer's public edPK from the
#      keychain, emitting rpidentity.json (schema: PairingIdentity::load).
#
# Key handling / anti-leak design (identical to export-keys.sh):
#   * Output lands ONLY inside ~/Library/Application Support/ac-dc (FileVault at
#     rest), never a synced/shared folder. Mode 600. Excluded from Time Machine.
#   * A one-shot LaunchAgent wipes the export (rpidentity.json + intermediates)
#     on your NEXT macOS login and removes itself. Survives one Linux session.
#   * Caveat: APFS copy-on-write means deletion is not a cryptographic erase;
#     FileVault-at-rest is the real protection. Re-generating is cheap.
#
# ⚠️ UNVERIFIED against a real iCloud keychain. The injection (SecItemAdd of a
# synchronizable item) and whether peers honour our synthesized identity are the
# open unknowns documented in macos/RPIDENTITY.md. Read the echoed steps.

set -euo pipefail

SOURCE="${BASH_SOURCE[0]}"
while [ -h "$SOURCE" ]; do
    DIR="$(cd -P "$(dirname "$SOURCE")" >/dev/null 2>&1 && pwd)"
    SOURCE="$(readlink "$SOURCE")"
    [[ $SOURCE != /* ]] && SOURCE="$DIR/$SOURCE"
done
SCRIPT_DIR="$(cd -P "$(dirname "$SOURCE")" >/dev/null 2>&1 && pwd)"

if command -v ac-dc-rpidentity-to-json >/dev/null 2>&1; then
    CONV="ac-dc-rpidentity-to-json"
else
    CONV="python3 \"$SCRIPT_DIR/rpidentity-to-json.py\""
fi
INJECTOR="$SCRIPT_DIR/inject-rpidentity.swift"

EXPORT_DIR="$HOME/Library/Application Support/ac-dc"
LABEL="app.ac-dc.cleanup-rpidentity"
AGENT_PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
CLEANUP_SH="$EXPORT_DIR/cleanup-rpidentity.sh"
DEVICE_LABEL="${AC_DC_DEVICE_LABEL:-ac-dc Linux}"

mkdir -p "$EXPORT_DIR"
chmod 700 "$EXPORT_DIR"
tmutil addexclusion "$EXPORT_DIR" 2>/dev/null || true

install_autowipe() {
    cat > "$CLEANUP_SH" <<CLEAN
#!/bin/bash
# One-shot: erase the ac-dc RPIdentity export, then remove self.
/bin/rm -f "$EXPORT_DIR/rpidentity.json" "$EXPORT_DIR/identity-self.json" \
           "$EXPORT_DIR/rp-dump.json" "$EXPORT_DIR/rpidentity.plist" 2>/dev/null
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
    echo "Auto-wipe armed: next macOS login deletes the RPIdentity export and removes the agent."
    echo "  (to wipe right now instead:  bash \"$CLEANUP_SH\" )"
}

secure_finish() {
    # $1 = path just written
    chmod 600 "$1"
    install_autowipe
    echo
    echo "Exported to: $1"
    echo "On Linux:    ./scripts/import-keys-from-macos.sh   (adapt REL to rpidentity.json)"
    echo
    echo "IMPORTANT: rpidentity.json carries OUR PRIVATE signing seed. Treat it like keys.json."
}

if [ "${1:-}" = "--arm-autowipe-only" ]; then
    [ -f "$EXPORT_DIR/rpidentity.json" ] && chmod 600 "$EXPORT_DIR/rpidentity.json"
    install_autowipe
    echo "Armed auto-wipe for $EXPORT_DIR/rpidentity.json"
    exit 0
fi

echo "== Step 1: read the same-account devices' public RPIdentity keys (peers) =="
echo

# If the caller already captured a Frida dump, use it directly (skip Path A).
if [ "${1:-}" = "--from-dump" ]; then
    [ -n "${2:-}" ] && [ -f "$2" ] || { echo "error: --from-dump needs a readable dump file" >&2; exit 1; }
    DUMP="$2"
    echo "Using provided dump: $DUMP"
elif security find-generic-password -s "RPIdentity-SameAccountDevice" -w >/tmp/rp_item.hex 2>/dev/null \
     && [ -s /tmp/rp_item.hex ]; then
    xxd -r -p /tmp/rp_item.hex > "$EXPORT_DIR/rpidentity.plist"
    rm -f /tmp/rp_item.hex
    echo "Path A captured ONE raw item -> $EXPORT_DIR/rpidentity.plist"
    echo "NOTE: the CLI returns only ONE item; use Path B to capture ALL peers."
    DUMP="$EXPORT_DIR/rpidentity.plist"
else
    rm -f /tmp/rp_item.hex 2>/dev/null || true
    cat <<EOF
Path A returned nothing (expected — synchronizable items are not readable via
the plain security CLI).

Path B: dump every RPIdentity peer via Frida-hooking rapportd (reliable):
  1. Disable SIP once (Recovery -> Terminal -> \`csrutil disable\`, reboot).
  2. pip3 install frida-tools      # or: brew install frida
  3. git clone https://github.com/seemoo-lab/apple-continuity-tools
  4. In System Settings -> General -> AirDrop & Handoff, turn Handoff OFF.
  5. sudo python3 apple-continuity-tools/keychain_access/keychain_access.py rapportd \\
         -o "$EXPORT_DIR/rp-dump.json"
  6. Turn Handoff back ON. rapportd re-reads the keychain; RPIdentity items
     (service RPIdentity-SameAccountDevice) print into rp-dump.json.
  7. Ctrl-D to stop.

Then re-run this script as:  export-rpidentity.sh --from-dump "$EXPORT_DIR/rp-dump.json"
EOF
    exit 0
fi

echo
echo "== Step 2: generate OUR keypair and inject its public half into iCloud keychain =="
if ! command -v swift >/dev/null 2>&1; then
    echo "error: 'swift' not found. Install Xcode Command Line Tools: xcode-select --install" >&2
    exit 1
fi
echo "Running inject-rpidentity.swift (SecItemAdd of a new RPIdentity-SameAccountDevice item)."
echo "  device label: $DEVICE_LABEL   (override with AC_DC_DEVICE_LABEL=...)"
if ! swift "$INJECTOR" --label "$DEVICE_LABEL" --out "$EXPORT_DIR/identity-self.json"; then
    echo >&2
    echo "Injection failed. If it was a missing-entitlement error you can try a" >&2
    echo "no-access-group item (its peer-trust status is unverified — see RPIDENTITY.md):" >&2
    echo "  swift \"$INJECTOR\" --no-access-group --label \"$DEVICE_LABEL\" --out \"$EXPORT_DIR/identity-self.json\"" >&2
    echo >&2
    echo "Or mint the key WITHOUT touching the keychain (peers will NOT accept us yet):" >&2
    echo "  swift \"$INJECTOR\" --dry-run --label \"$DEVICE_LABEL\" --out \"$EXPORT_DIR/identity-self.json\"" >&2
    exit 1
fi
chmod 600 "$EXPORT_DIR/identity-self.json"

echo
echo "== Step 3: assemble rpidentity.json (our seed + peer edPKs) =="
eval $CONV --self "\"$EXPORT_DIR/identity-self.json\"" --dump "\"$DUMP\"" -o "\"$EXPORT_DIR/rpidentity.json\""
rm -f "$EXPORT_DIR/rp-dump.json" "$EXPORT_DIR/rpidentity.plist" "$EXPORT_DIR/identity-self.json"  # plaintext peers + our seed copy
secure_finish "$EXPORT_DIR/rpidentity.json"
