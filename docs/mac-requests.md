# Requests for the macOS side

This file is written to be pasted whole into a Claude session on a Mac. It needs no other context.

## Background

`ac-dc` runs on an Asahi Linux laptop (same Wi-Fi LAN as this Mac) and talks Apple's Universal Clipboard. Mac -> Linux works end to end: the Linux side sees the Mac's BLE `0x0c` Handoff advert, dials the Mac's `_companion-link._tcp` service, does Pair-Verify and pulls the pasteboard.

Linux -> Mac does not work. `ac-dc auto` (a) on every Linux copy seals a 16-byte `0c 0e 08 iv0 iv1 tag ct[10]` Handoff advert with one of the iCloud-synced Continuity keys it holds (always the first key in its key file) with the clipboard bit set, and broadcasts it over BLE for 20 seconds; (b) publishes once at startup a Companion-Link service named `ac-dc` over mDNS with TXT keys `rpMd`, `rpVr`, `rpFl`, `rpHN` (`auto` passes no BLE address, so `rpBA` is not published today), no `rpAD`, `rpHA`, `rpHI` or `rpMac`. The Mac never opens a TCP connection to the Linux server, and Cmd+V on the Mac does not paste the Linux text.

Hypothesis, unproven: the Mac decrypts the advert as coming from an existing account device, then looks for that device's Companion-Link service (matching `rpAD`/`rpBA` against that device's IRK) and expects that device's Ed25519 identity in Pair-Verify. Our service matches neither, so the Mac never dials it. These requests test that.

## Privacy rules for every reply

Never send keys, `keys.json`, `rpidentity.json`, `rpAD`/`rpBA` values, pairing UUIDs or clipboard contents. Copy a throwaway string such as `test123`. Strip hex blobs, MAC and IP addresses, UUIDs, device names and account IDs from any log line. Prefer a short summary over pasted lines. Report booleans and error codes.

## 0. Ask the Mac first

Which device does the Continuity key the Linux side broadcasts with belong to: this Mac itself, an iPhone, an iPad or another Mac? The Linux side always uses the first entry of its `keys.json`. The user will give you the first 8 characters of that entry's `id`. That id is the keychain item's `keyIdentifier`, or its label (`labl`) when that is absent, so match the item on either attribute and report which device its attributes name. Read attributes only: never run `security` with `-w` or `-g`, and never print the secret. Report the device name only. If it is this Mac's own key, the Mac probably ignores the advert, which would explain everything and needs no other test.

## Linux setup (user present)

On the Linux laptop, from a Wayland session with `wl-clipboard` installed and Bluetooth powered: `ac-dc -v auto`. Ask the user to confirm `echo $WAYLAND_DISPLAY` prints something non-empty in that same shell before starting it: if it's unset, `ac-dc` logs "skipping Linux -> Mac clipboard watcher" and keeps running without ever watching the clipboard, instead of exiting with an error. With `-v` it logs, at debug level, the exact TXT record it publishes (identifying values replaced by their length) and, per copy, the advert's cleartext status byte, flags byte, counter and length. Ask the user to read those lines out if a result looks odd. A Linux copy broadcasts only when the clipboard is non-empty and was not just pulled from the Mac, and the advert lasts 20 seconds.

## 1. Does the Mac see and dial the Linux Companion-Link server?

Mac:

1. `dns-sd -B _companion-link._tcp local.`; stop it with Ctrl-C after 10 seconds. Report whether an instance named `ac-dc` appears (yes/no).
2. In one terminal run `sudo tcpdump -n -i any 'host <linux-ip> and tcp[tcpflags]&(tcp-syn|tcp-rst)!=0'` (the user supplies `<linux-ip>`) and in a second `log stream --level debug --style compact --predicate 'process == "rapportd"'`, both for 60 seconds, while the user copies `test123` on Linux. Report whether the Mac sent a SYN to the Linux host (yes/no) and whether the Linux host answered with SYN-ACK or RST (the server listens on IPv4 only; a SYN-ACK shows as SYN plus ACK, a refusal as RST).
3. If the Mac sent a SYN and got SYN-ACK, summarize from the `rapportd` log where the connection ends: Pair-Verify identity lookup, `_systemInfo` or payload request, with any error code.
4. If the Mac sent no SYN, summarize any `rapportd` line about the `ac-dc` instance or about skipping a device (for example no matching identity or IRK).

## 2. Does the Mac react to the Linux BLE `0x0c` broadcast?

Mac: within 20 seconds of the Linux copy, press Cmd+V in a text field. Report whether `test123` pasted (yes/no) and roughly how long it took. During the same copy, run `log stream --level debug --style compact --predicate 'process == "sharingd"'` and summarize lines about `Handoff`, `pasteboard` or `UniversalClipboard`, including any `-6708`.

## 3. Does the Mac accept the identifier our server presents?

Only if item 1 shows a connection. When `rpidentity.json` has a `pairing_id`, our Pair-Verify M2 and `_systemInfo` carry it. Report whether the Mac accepted it or rejected the pairing (look for `identifier`, `NotPaired` or `-6708` in the item 1 and 2 captures).
