# ac-dc

Apple Universal Clipboard and AirDrop-compatible file-transfer tools for Linux, written in Rust.

- **Universal Clipboard (macOS 26.1+ <-> Linux):** Decrypts Apple `0x0c` Continuity BLE copy announcements, pairs with `rapportd` over LAN or `awdl0` via `PairSetup` (`SessionPaired`) without disabling SIP or touching the macOS Data Protection Keychain, pulls pasteboards via `com.apple.handoff.payload-request` onto `wl-copy`, and serves Linux Wayland copies back over `_companion-link._tcp.local.` + `0x0c` BLE adverts.
- **AirDrop-Compatible Transfers:** Discover nearby Apple devices, send and receive files or HTTP(S) links over AWDL (`Everyone` and `Contacts Only` modes), with disk-space quotas and collision-safe naming.
- **Desktop Integration:** Split root radio broker (`ac-dc-daemon.service`) + user protocol daemon (`ac-dc-user.service`), an event-driven Omarchy panel (`Panel.qml`), and a native GPUI desktop window (`ac-dc ui`).

---

## Project Status

| Feature | Status | Notes |
| --- | --- | --- |
| **Mac -> Linux Universal Clipboard** | Live-verified (`macOS 26.1`) | `PairSetup M1..M6`, `PairVerify M1..M4`, `_systemInfo`, `pbpaste2`, `UASharedPasteboardInfoWrapper`, `wl-copy` |
| **Linux -> Mac Universal Clipboard** | Wired & smoke-tested | `_companion-link._tcp.local.` mDNS/TCP server + `wl-paste --watch` + `0x0c` BLE broadcaster (`hci0`) |
| **AirDrop-compatible Send / Receive** | Interop-tested in Rust | Everyone & Contacts Only (`SenderRecordData` / `ReceiverRecordData`), DVZip/CPIO; real Apple AWDL delivery depends on `brcmfmac-awdl` |
| **Event-Driven Daemon & Omarchy UI** | Implemented | `ac-dc ctl subscribe` JSONL stream, zero-polling `Panel.qml`, GPUI `ac-dc ui` |

> **AWDL note:** The `brcmfmac` AWDL kernel/driver tooling lives in the sibling [brcmfmac-awdl](https://github.com/cristim/brcmfmac-awdl) repository (`awdlctl`). Universal Clipboard works over ordinary Wi-Fi/LAN (`_companion-link._tcp.local.`) even when `awdl0` is down, falling back to `awdl0` if present.

---

## Build & Test

Requires Rust, D-Bus, and OpenSSL development headers. Runtime BLE uses BlueZ (`bluetoothd`); Wayland clipboard sync uses `wl-copy` and `wl-paste` from `wl-clipboard`.

```sh
cargo build --release --locked
cargo test --locked
./target/release/ac-dc --help
```

Tests run against loopback TCP/TLS and synthetic archives; they never touch real Apple keys, hardware radios, or your desktop clipboard.

---

## Install (Arch Linux / Omarchy)

Build and install the package with `makepkg -si`, then enable the split system (radio) and user (protocol/clipboard) services:

```sh
sudo systemctl enable --now ac-dc-daemon.service
systemctl --user enable --now ac-dc-user.service
ac-dc ctl status
```

See [packaging/INSTALL.md](packaging/INSTALL.md) for firewall/UFW and `wheel` group details.

---

## Universal Clipboard Quick Start

Keys and pairing identities default to `~/.local/share/ac-dc/` (mode `0600`), with automatic fallback to `keys.json` / `rpidentity.json` in the current working directory.

### 1. Export & Transfer Continuity Keys

Export your iCloud-synced Continuity BLE keys on macOS (see `macos/export-keys.sh` and [macos/ac-dc-send/README.md](macos/ac-dc-send/README.md)), then receive them over encrypted BLE SAS on Linux:

```sh
ac-dc receive-key
```

This writes `~/.local/share/ac-dc/keys.json` (mode `0600`). If `keys.json` includes `ids_device_id`, `receive-key` automatically runs Companion-Link pairing over LAN mDNS as well.

### 2. One-Time Companion-Link Pairing

If you transferred `keys.json` manually (or need to specify an account IDS device UUID so `sharingd` recognizes your sender in `IDSService.devices`), run:

```sh
ac-dc pair --pairing-id <IDS-DEVICE-UUID>
```

`ac-dc pair` auto-discovers your Mac over `_companion-link._tcp.local.`, completes `PairSetup M1..M6` (`_pwTy=0`, `_auTy=8`, `fixedPIN="public"`), and saves `~/.local/share/ac-dc/rpidentity.json` (mode `0600`). Use `--host` and `--port` to target a specific address.

### 3. Pull Once or Run Continuous Bidirectional Sync

```sh
# Pull the Mac's current clipboard onto Wayland (auto-discovers over LAN, then awdl0)
ac-dc pull

# Or run continuous bidirectional sync (BLE scanner + Companion-Link server + wl-paste watcher)
ac-dc auto --notify
```

When `~/.local/share/ac-dc/keys.json` and `~/.local/share/ac-dc/rpidentity.json` are both present, `ac-dc daemon` (`ac-dc-user.service`) starts clipboard auto-sync automatically and surfaces its state (`watching`, `synced`, `paused`, `unpaired`) in the Omarchy panel.

### Developer Demos & Diagnostics

```sh
ac-dc pull --loopback           # Exercise full Pair-Verify -> OPACK -> wl-copy flow offline
ac-dc auto --loopback           # Same via orchestrate loop
ac-dc scan                      # Decrypt live 0x0c BLE Handoff adverts
ac-dc capture                   # Print raw 0x0c adverts as hex (no keys needed)
ac-dc discover --lan            # Browse _companion-link._tcp.local. over LAN
ac-dc doctor --json             # Check radio, keys, identity, and Wayland prerequisites
ac-dc debug                     # Inspect system/user daemons, sockets, and recent logs
```

---

## AirDrop-Compatible Transfers

Open the Omarchy bar widget or launch `ac-dc ui` for recipient discovery, file/link sending, incoming approval prompts, transfer progress, and Settings (`Device name`, `Receive folder`, `Bluetooth wake`, `Copy received to clipboard`, `Max free disk %`).

CLI equivalents:

```sh
ac-dc ctl peers
ac-dc ctl receive --mode timed --seconds 600 --name Omarchy --directory ~/Downloads/Adhoc
ac-dc ctl send --host 'fe80::PEER%awdl0' --port 8770 --name Linux --file /path/to/file
```

For `Contacts Only` mode, manage trusted Apple IDs/phone numbers with `ac-dc contacts`:

```sh
ac-dc contacts add user@example.com
ac-dc contacts list
```

---

## Documentation

- [packaging/INSTALL.md](packaging/INSTALL.md): Arch package installation, systemd units, and firewall setup.
- [docs/architecture.md](docs/architecture.md): Companion-link, AWDL, and security architecture.
- [macos/RPIDENTITY.md](macos/RPIDENTITY.md): `RPIdentity` / `CUPairedPeer` research notes.
- [docs/clipboard-research.md](docs/clipboard-research.md): Early clipboard protocol audit.

## License

MIT. Not affiliated with Apple. Intended only for your own devices and Apple ID.
