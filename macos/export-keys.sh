#!/bin/bash
# export-keys.sh — run this ONCE under macOS (booted natively, signed into the
# same Apple ID as your iPhone) to export the Continuity BLE encryption keys
# that handoff-clip needs on the Linux side.
#
# The keys are long-term and iCloud-synced, so you only need to re-run this if
# they rotate (rare) or you add a device. Copy the resulting keys.json to your
# Linux/Asahi partition.
#
# There are two extraction paths. Try A first; if it comes up empty, use B.

set -euo pipefail
OUT="${1:-keys.json}"

echo "== Path A: security CLI (works if the items are readable in your login keychain)"
# The Continuity keys are generic-password items under this service. They are
# synchronizable + protected, so the plain security CLI may or may not return
# them depending on macOS version and ACL. We ask for the raw data blob (a
# binary plist) and let the Rust side parse it.
if security find-generic-password -s "com.apple.continuity.encryption" -w >/tmp/hc_key.hex 2>/dev/null; then
    xxd -r -p /tmp/hc_key.hex > /tmp/hc_key.plist
    plutil -convert xml1 /tmp/hc_key.plist -o /tmp/hc_key.xml
    echo "Exported one raw keychain item to /tmp/hc_key.plist"
    echo "NOTE: security CLI returns only ONE item. If you have multiple devices,"
    echo "      use Path B to capture all keys."
    # Emit our JSON with just this one; keyData is inside the plist, so we hand
    # the raw plist to handoff-clip instead:
    cp /tmp/hc_key.plist "${OUT%.json}.plist"
    echo "Wrote ${OUT%.json}.plist — pass THAT file to handoff-clip (it parses plists too)."
    rm -f /tmp/hc_key.hex
    exit 0
fi

cat <<'EOF'
Path A returned nothing (expected on recent macOS — the item ACL blocks the
plain security CLI).

== Path B: dump every key via Frida-hooking rapportd (reliable) ==

This uses seemoo-lab's keychain_access tool, which hooks SecItemCopyMatching in
rapportd and prints every Continuity key for all your iCloud devices.

  1. Disable SIP once (Recovery -> Terminal -> `csrutil disable`, reboot).
  2. pip3 install frida-tools    # or: brew install frida
  3. git clone https://github.com/seemoo-lab/apple-continuity-tools
  4. In System Settings -> General -> AirDrop & Handoff, turn Handoff OFF.
  5. sudo python3 apple-continuity-tools/keychain_access/keychain_access.py rapportd -o dump.json
  6. Turn Handoff back ON. rapportd reloads the keys; they print into dump.json.
  7. Ctrl-D to stop.

Then convert dump.json to handoff-clip's keys.json with the bundled converter
(pure Python stdlib; runs on macOS or Linux):

    python3 "$(dirname "$0")/dump-to-keys.py" dump.json -o keys.json

  dump-to-keys.py walks the whole dump, decodes every hex/base64/plist blob it
  finds, and keeps the ones that parse to a keychain item carrying `keyData`
  (service "com.apple.continuity.encryption"). It skips wrapped keys (not
  directly usable) and unrelated items automatically, and emits:

    { "keys": [ { "id": "<keyIdentifier>", "key": "<keyData as hex>" }, ... ] }

  Add --include-wrapped to inspect wrapped keys if the usable set comes up empty.

Copy keys.json to your Linux partition and run:  handoff-clip scan --keys keys.json
EOF
exit 1
