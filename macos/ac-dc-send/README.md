# `ac-dc send-key` (macOS)

The macOS half of the Bluetooth LE key transfer: it reads the Continuity keys
out of this Mac's keychain and pushes them to a Linux box running `ac-dc
receive-key`. This is an alternative to copying `keys.json` across a read-only
APFS mount — handy when the Mac and the Linux box are two different machines.

`send-key` does the export itself, so there is no separate export step and the
keys never touch the disk. `export-keys` writes the same data to a file for the
dual-boot flow. Both use `SecItemCopyMatching`, which beats
[`../export-keys.sh`](../export-keys.sh) Path A (that returns only the *first*
item) and needs neither SIP disabled nor Frida (Path B).

> ✅ **Validated end to end on real hardware**: keychain export on macOS 26.1,
> then a full BLE transfer to a Linux `ac-dc receive-key` with matching security
> codes and `keys.json` written on the far side. The security-relevant code (X25519 handshake, HKDF-SHA512 session
> key, 6-digit SAS, ChaCha20-Poly1305, chunk framing) mirrors the Rust reference
> in [`src/transfer.rs`](../../src/transfer.rs), which **is** unit-tested; the
> CoreBluetooth plumbing is a careful draft. Expect to fix things the Swift
> compiler or a real device flags.

## Build

Requires the Xcode Command Line Tools (`xcode-select --install`).

```sh
cd macos/ac-dc-send
swift build -c release
codesign --force --sign - .build/release/ac-dc
```

### Making "Always Allow" actually stick

The keychain ACL grants access to a *code identity*, not to a path. With an
ad-hoc signature (`--sign -`) that identity is the binary's own hash, so every
rebuild produces a new identity and macOS asks again, no matter how many times
you answer "Always Allow".

To make the grant survive rebuilds, sign with a stable self-signed identity:
create a code-signing certificate once (Keychain Access > Certificate Assistant
> Create a Certificate, type "Code Signing", self-signed), then sign with its
name instead:

```sh
codesign --force --sign "My Code Signing Cert" .build/release/ac-dc
```

The designated requirement then names the certificate rather than one exact
binary, so a rebuild keeps the access you granted.

Failing that, two ways to avoid the prompts entirely:

- `--key-id <uuid>` fetches a single key, which is one query and so one prompt
  instead of one per key.
- `send-key --keys <path>` reads an already-exported file and never touches the
  keychain.

The executable product is named `ac-dc` (at
`.build/release/ac-dc`). You may symlink or copy it onto your `PATH`.
Re-sign after each rebuild.

## Test

```sh
swift test
```

`TransferInteropTests` pins the session key, SAS and frame layout to vectors
produced by the Rust implementation in [`src/transfer.rs`](../../src/transfer.rs).
A failure there means the two ends have diverged and the transfer will break or
show mismatched security codes, so regenerate the vectors from the Rust side
rather than editing the expectations. `KeychainExportTests` covers item decoding
and the `keys.json` schema that `src/keystore.rs` loads.

## Usage

```sh
# On Linux:
ac-dc receive-key            # starts advertising, waits

# On this Mac:
ac-dc send-key               # exports from the keychain, scans, connects, transfers
```

The first run raises a keychain prompt per key ("ac-dc wants to use your
confidential information"); answer **Always Allow**. Nothing is written to disk:
the keys go from the keychain into memory and out over the encrypted BLE link.

To export to a file instead (the dual-boot flow, where Linux reads `keys.json`
off a read-only APFS mount):

```sh
ac-dc export-keys                          # ~/Library/Application Support/ac-dc/keys.json, mode 0600
ac-dc export-keys -o /path/to/keys.json
bash ../export-keys.sh --arm-autowipe # one-shot wipe on next login
ac-dc send-key --keys /path/to/keys.json   # send an already-exported file
```

Both ends print a **6-digit security code (SAS)**. Compare them: they must be
identical. On the Linux side, only answer `y` to write `keys.json` once you have
confirmed the codes match — this is what defeats a Bluetooth man-in-the-middle.
Any other argument (or none) prints usage.

## What it does

0. Reads every generic-password item with service
   `com.apple.continuity.encryption` from the keychain (two passes: enumerate
   accounts, then fetch each item's data — the legacy file-based keychain
   rejects `kSecMatchLimitAll` together with `kSecReturnData`), decodes the
   binary plist for `keyData`/`keyIdentifier`, drops wrapped keys, and builds
   the `keys.json` bytes in memory. See
   [`Sources/ac-dc/KeychainExport.swift`](Sources/ac-dc/KeychainExport.swift).
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
