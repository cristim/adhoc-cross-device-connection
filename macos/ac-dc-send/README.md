# `ac-dc send-key` (macOS)

The macOS half of the Bluetooth LE key transfer: it reads the exported
`keys.json` and pushes it to a Linux box running `ac-dc receive-key`. This is an
alternative to copying `keys.json` across a read-only APFS mount — handy when
the Mac and the Linux box are two different machines.

> ⚠️ **Unvalidated.** This helper has **not** been compiled or run against real
> hardware from this repository (it was written on a Linux box with no Swift
> toolchain). The security-relevant code (X25519 handshake, HKDF-SHA512 session
> key, 6-digit SAS, ChaCha20-Poly1305, chunk framing) mirrors the Rust reference
> in [`src/transfer.rs`](../../src/transfer.rs), which **is** unit-tested; the
> CoreBluetooth plumbing is a careful draft. Expect to fix things the Swift
> compiler or a real device flags.

## Build

Requires the Xcode Command Line Tools (`xcode-select --install`).

```sh
cd macos/ac-dc-send
swift build -c release
```

The executable product is named `ac-dc` (at
`.build/release/ac-dc`). You may symlink or copy it onto your `PATH`.

## Usage

First export the keys on this Mac (see [`../export-keys.sh`](../export-keys.sh)),
which writes `~/Library/Application Support/ac-dc/keys.json`. Then:

```sh
# On Linux:
ac-dc receive-key            # starts advertising, waits

# On this Mac:
ac-dc send-key               # scans, connects, transfers
```

Both ends print a **6-digit security code (SAS)**. Compare them: they must be
identical. On the Linux side, only answer `y` to write `keys.json` once you have
confirmed the codes match — this is what defeats a Bluetooth man-in-the-middle.
Any other argument (or none) prints usage.

## What it does

1. Generates an ephemeral X25519 keypair.
2. Scans for the custom key-transfer GATT service, connects, and **reads** the
   receiver's public key.
3. Derives the session key (HKDF-SHA512 over the ECDH secret, bound to the
   transcript) and the SAS, and prints the SAS.
4. **Writes** its own public key, then streams the ChaCha20-Poly1305-sealed
   `keys.json` as length-prefixed frames sized to the GATT MTU.

The exact byte layout is documented in
[`Sources/ac-dc/Transfer.swift`](Sources/ac-dc/Transfer.swift), mirrored verbatim
from the Rust `src/transfer.rs`.
