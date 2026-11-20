# Requests for the macOS side

Open questions that only a real Mac can answer. Each item says what to run, what to capture and what to send back.

**Privacy rules for every reply.** Never send keys, `keys.json`, `rpidentity.json`, `rpAD`/`rpBA` values, pairing UUIDs or clipboard contents. Copy a throwaway string such as `test123` on both machines. Strip hex blobs, MAC and IP addresses, UUIDs, device names and account IDs from any log line. Prefer a short summary of what the lines show over pasting them.

Status of the Mac -> Linux direction: live-verified (Pair-Verify M1..M4, `pbpaste2` pull). The Linux -> Mac direction below is wired and smoke-tested on Linux only.

## Linux setup (user present)

`ac-dc -v auto` from a Wayland session (`WAYLAND_DISPLAY` set), with `keys.json`, `rpidentity.json` and `wl-clipboard` installed and Bluetooth powered. If any of the first three is missing, `auto` exits early; report that instead of continuing.

A Linux copy triggers a BLE `0x0c` broadcast only when the clipboard is non-empty and was not just pulled from the Mac. The advert lasts 20 seconds.

## 1. Does the Mac see and dial the Linux Companion-Link server?

Mac:

1. `dns-sd -B _companion-link._tcp local.`; stop it with Ctrl-C after 10 seconds. Report whether an instance named `ac-dc` appears (yes/no).
2. In one terminal run `sudo tcpdump -n -i any 'tcp[tcpflags]&(tcp-syn|tcp-ack)==tcp-syn and dst host <linux-ip>'` and in a second `log stream --level debug --style compact --predicate 'process == "rapportd"'`, both for 60 seconds, while the user copies `test123` on Linux. Report whether the Mac sent a SYN to the Linux host (yes/no) and whether the Linux host answered with SYN-ACK or RST (the server listens on IPv4 only).
3. If the Mac sent a SYN and got SYN-ACK, summarize from the `rapportd` log where the connection ends: Pair-Verify identity lookup, `_systemInfo` or payload request, with any error code.

## 2. Does the Mac react to the Linux BLE `0x0c` broadcast?

Mac: within 20 seconds of the Linux copy, press Cmd+V in a text field. Report whether `test123` pasted (yes/no) and roughly how long it took. During the same copy, run `log stream --level debug --style compact --predicate 'process == "sharingd"'` and summarize lines about `Handoff`, `pasteboard` or `UniversalClipboard`, including any `-6708`.

## 3. Does the Mac accept the identifier our server presents?

Only if item 1 shows a connection. When `rpidentity.json` has a `pairing_id`, our Pair-Verify M2 and `_systemInfo` carry it. Report whether the Mac accepted it or rejected the pairing (look for `identifier`, `NotPaired` or `-6708` in the item 1 and 2 captures).
