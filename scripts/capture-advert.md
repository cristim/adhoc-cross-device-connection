# Capturing a real Handoff / Universal Clipboard advert (for validation)

Our AES-GCM path (2-byte IV, 1-byte tag) is derived from the spec and the
seemoo-lab code but **unvalidated against real traffic**. To confirm it — and to
pin down IV byte-order and TLV offsets on your current iOS/macOS — capture one
real advert while your iPhone copies something, then feed it to
`ac-dc decrypt`.

There are two ways to grab the bytes. Use method 1; method 2 is a manual
cross-check.

## Method 1 (recommended): `ac-dc capture`

This reuses our own, tested advert parser and prints each Handoff advert as hex
already normalized to the `0c ..` TLV that `decrypt --data` expects. No keys
needed, no root beyond what BlueZ already allows.

```bash
# Terminal 1: start capturing (no keys required)
ac-dc capture

# Then, on the iPhone (unlocked, Handoff ON, near this machine):
#   copy some text (e.g. in Notes) — this sets the "clipboard available" flag.
```

You'll see lines like:

```
E2:AB:..:..  0c0e08 2a00 99 <10 bytes ct>   # raw_mfg=... status=0x08 iv=2a00 tag=99 ct=...
```

Copy the second field (the `0c0e…` hex) and validate once you have `keys.json`:

```bash
ac-dc decrypt --keys keys.json --data 0c0e08...
# success => "decrypted with key <id>: ...  clipboard_available=true"
```

If no key authenticates, the crypto assumptions need a tweak (most likely the IV
byte order or the J0 derivation) — capture a few adverts and we iterate on them
offline; no device access needed after that.

## Method 2 (cross-check): `btmon`

`btmon` is BlueZ's HCI monitor. It shows the raw advertising reports the
controller receives. Caveat: **newer BlueZ decodes Apple Continuity itself and
may not print the raw hex**, so treat this as an eyeball check, not the primary
extractor.

```bash
# Terminal 1: monitor HCI (needs root). Write a lossless capture too.
sudo btmon -w /tmp/hci.snoop

# Terminal 2: active-scan so the controller emits advertising reports
bluetoothctl
  [bluetooth]# menu scan
  [bluetooth]# transport le
  [bluetooth]# back
  [bluetooth]# scan on
# ...now copy something on the iPhone...
  [bluetooth]# scan off
  [bluetooth]# exit
```

In Terminal 1's output look for an LE Advertising Report containing:

```
Company: Apple, Inc. (76)
  ... Type: Handoff (0x0c) ...     # (decoded by newer btmon)
```

If your btmon prints the raw manufacturer data as hex, the Handoff TLV is the
run starting `0c 0e …` (type 0x0c, length 0x0e = 14 bytes). That's the same hex
`ac-dc capture` gives you. Replay a saved capture later with:

```bash
btmon -r /tmp/hci.snoop
```

## What we're validating

For a "clipboard available" advert the decrypted 10-byte payload should have the
flags byte with bit `0x08` set. Confirming that from a real packet is what turns
Milestone 1 from "plausible" into "verified".
