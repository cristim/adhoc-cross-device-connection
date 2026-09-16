# ac-dc

Apple Continuity and clipboard tools for Linux, implemented in Rust.

- Observe and decrypt Universal Clipboard BLE announcements using keys exported
  from your own macOS account.
- Pull a pasteboard through companion-link, with mandatory peer signature
  verification, scoped IPv6 discovery, and Wayland clipboard output.
- Send and receive Airdrop-compatible files/links through a native Rust GTK UI or the CLI.
- Inspect prerequisites and advertise Apple BLE TLVs for controlled experiments.

**Compatibility is experimental.** Companion-link framing and request fields
still need validation against Apple devices. Rust-to-Rust Airdrop-compatible transfers, including user approval/decline, pass local TLS
tests. Discovery and HTTPS `/Discover` have succeeded against a real Mac over
BCM4378 AWDL. File-transfer interoperability is still being tested; do not
interpret discovery alone as a completed transfer.

## Separate driver repository

The patches and Linux radio tools now live in **`../brcmfmac-awdl/`**, normally
`~/src/brcmfmac-awdl`. That repository retains the driver history and the newer
patches from `feat/awdl-4378-data-path`. It provides Rust `awdlctl`; it owns
interface creation, firmware iovars, peer registration, channel schedules, and
bounded radio/band management. There is no Rust path dependency between repos.

`ac-dc` runs as your desktop user. Configure AWDL with the driver tools separately;
it never installs/reloads a driver or silently changes your Wi-Fi connection.

## Build

Requires Rust, system D-Bus and OpenSSL development libraries. Runtime BLE uses
BlueZ; clipboard output uses `wl-copy` from `wl-clipboard`.

```sh
cargo build --release --locked
cargo test --locked
./target/release/ac-dc --help
```

## Install and quick start

On Arch Linux, build the package from this repository with `makepkg -si`.
The package installs the `ac-dc` command, the GTK launcher, the Omarchy
systray widget, and the restricted root service used for radio and transfer
operations. Enable the service once after installation:

```sh
sudo systemctl enable --now ac-dc-daemon.service
ac-dc ui
```

The widget and the CLI talk to the service through its local Unix socket; no
network control port is exposed. See [packaging/INSTALL.md](packaging/INSTALL.md)
for firewall and first-transfer guidance.

Tests use temporary data and loopback TCP/TLS. They never access real Apple keys,
change the radio, or change the desktop clipboard.

## Universal Clipboard

```sh
ac-dc doctor --json --keys /path/to/keys.json --identity /path/to/rpidentity.json
ac-dc status --json
ac-dc scan --keys /path/to/keys.json
ac-dc capture
ac-dc discover                  # awdl0 only, scoped IPv6 addresses
ac-dc discover --lan            # ordinary LAN experiment
ac-dc pull --host 'fe80::1234%awdl0' --port 49152 --identity /path/to/rpidentity.json
ac-dc auto --keys /path/to/keys.json --identity /path/to/rpidentity.json --notify
```

`auto` waits for real BLE copy events. It serializes pulls, bounds the event queue
and transaction time, and matches the triggering BLE address against mDNS `rpBA`.
Use `--instance FULL_MDNS_INSTANCE` if that address correlation does not work on
your OS version. Discovery hints never replace Pair-Verify authentication.
The BLE tag is only one byte; a successful BLE decode alone is not strong proof
of device identity. An unknown Pair-Verify signing key stops the connection.

Management-frame discovery is an optional fallback:

```sh
sudo awdlctl events --seconds 30 > /tmp/awdl-events.jsonl
ac-dc pull --identity /path/to/rpidentity.json \
  --events /tmp/awdl-events.jsonl --peer-mac aa:bb:cc:dd:ee:ff
```

Only companion-link SRV records for that AWDL MAC, interface, and the preceding
30 seconds are accepted. This is useful if ordinary mDNS fails; whether a given
Apple device publishes companion-link in those records must be measured.
BLE MACs and AWDL MACs are different identifiers.

`ac-dc pull --loopback` and `ac-dc auto --loopback` are developer demos and **do
write** the mock pasteboard to your clipboard. The unit tests do not.

## Airdrop-compatible UI and sending

Launch **Adhoc Cross-Device Connection** from the application launcher, or run `ac-dc ui`.
The native GTK window provides device name and destination settings, a bounded
receiving session, incoming Accept/Decline prompts, recipient discovery, a file
chooser and link sending. It invokes the installed root-owned radio service
through desktop authentication; all transfer handling runs as your normal user.

Set the receiving Apple device to **Everyone for 10 Minutes**. Click **Find
recipients**, select the intended device, select files or enter an HTTP(S) link,
and send. Nearby names are discovery hints, not authenticated Apple identities.
Everyone-mode TLS uses self-signed certificates; Contacts Only is not implemented.

CLI equivalents (prepare the radio service first):

```sh
sudo systemctl start ac-dc-daemon.service
ac-dc air-drop-peers --tls-identity ~/.local/share/ac-dc/airdrop
ac-dc send --host 'fe80::PEER%awdl0' --port 8770 \
  --tls-identity ~/.local/share/ac-dc/airdrop --name Linux /path/to/file
# For a link, replace the filename with --url https://example.com/
```

Sending uses scoped HTTPS Discover/Ask/Upload, with DVZip-wrapped CPIO over
chunked HTTP and one transfer UUID shared by Ask and Upload.
Select regular files: folders and symlinks are currently rejected, duplicate
basenames must be renamed, and a transfer is bounded to 64 MiB. Receiving
flattens archive paths and never overwrites existing files. Larger/streaming
transfers and directory-preserving transfers remain follow-up work.

## Airdrop-compatible receive (CLI)

Prepare an AWDL window and peer registration using `awdlctl`, then run as your
normal user:

```sh
ac-dc receive --directory ~/Downloads/Adhoc \
  --tls-identity ~/.local/share/ac-dc/airdrop \
  --name "Linux Mac" --seconds 600 --once --notify
```

This explicitly accepts nearby **Everyone-mode** transfers during the window.
It generates a persistent local TLS identity on first use, advertises
`_airdrop._tcp` over awdl0, and exits after the first completed transfer with
`--once`, at timeout, or on SIGINT/SIGTERM. It does not implement Contacts Only.

Supports chunked HTTP, Ask-only URL transfers, DVZip/zlib/gzip payloads, and
odc/newc CPIO regular files. Names are flattened to the destination directory;
symlinks/devices are skipped, collisions never overwrite existing files, and
compressed/decompressed sizes and connection counts are limited. URLs are saved
as text and never opened automatically. Text and recognized images are copied
to the clipboard; other files are saved. Notifications are optional.

The separate Rust driver helper now publishes firmware service/PSF templates,
tracks election changes, registers nearby peers, and sends scoped mDNS
announcements. `ac-dc-daemon.service` owns this bounded session. Stopping
it withdraws host discovery and brings the AWDL host link down; firmware template
state remains until driver reload because disabling a populated template can
hang this firmware. The service reuses that state on its next start.

The older `awdl-window.service` is useful for plain clipboard/radio experiments;
it cannot replace the protocol service. Do not run both at once.

## BLE advertising experiments

```sh
ac-dc advertise --airdrop-wake --seconds 30
ac-dc advertise --data HEX_APPLE_TLVS --seconds 10 --interval-ms 100
```

Registration and cancellation use BlueZ; no raw HCI commands compete with
bluetoothd. `--airdrop-wake` emits an Airdrop-compatible type-5 advert, not a Universal
Clipboard announcement. The raw mode sends supplied bytes without allocating
AES nonces or inventing a usable same-account identity. Full Linux → Apple
Universal Clipboard still requires a validated server and nonce ownership.

## Keys and macOS tools

Keep keys outside source control, readable only by your user. `keys.json`,
RPIdentity exports, and dump files are ignored. Never share them in bug reports.

- `macos/export-keys.sh`: existing macOS Continuity key exporter and converters.
- [macos/RPIDENTITY.md](macos/RPIDENTITY.md): RPIdentity export and trust research.
- [macOS helper](macos/ac-dc-send/README.md): Swift/CoreBluetooth key transfer.
  Linux receives with `ac-dc receive-key --out /path/to/keys.json`; compare the
  security code on both devices before accepting.
- `macos/awdl-capture.sh`: capture real protocol framing on macOS for validation.

These existing macOS integration tools are retained; all new Linux runtime
features and the new driver userspace tools are Rust.

See [architecture and remaining work](docs/architecture.md) and
[Omdrop provenance](docs/provenance.md). Optional user-service template:
`packaging/ac-dc.service`. It is not installed or enabled by building.

## License

MIT. Not affiliated with Apple. Uses keys for your own devices/account.

## Research and validation

- [Clipboard protocol audit](docs/clipboard-research.md)
- [Sapporo issue review](docs/sapporo-research.md)

The clipboard audit found concrete wire-format problems. Header/AAD framing and
OPACK byte order/reference decoding have now been corrected; real UC request
payloads, keyed-archive parsing and current-device authentication still need work.

## 0.1.0-0 release notes

See [installation and operating limits](packaging/INSTALL.md) and
[OWL, OpenDrop and LocalSend findings](docs/owl-opendrop-localsend-research.md).
The Rust UI adds outgoing progress/cancellation, folder selection and a renewable
radio countdown. Archives stream through temporary files; completed top-level
items are published directly in the receive directory, with Finder-style numeric
collision names. The sender separates discovery from the Ask/Upload connection,
following OpenDrop's CLI sequence.

Receiving was confirmed during development after the scoped UFW rule was
added. Outgoing Apple-device delivery and Universal Clipboard remain unconfirmed;
local TLS tests are not a substitute for those interoperability checks.


## Dedicated sender

The sender now follows OpenDrop's dedicated connection flow: discovery is
separate, then Ask and Upload share an OpenSSL/HTTP1 connection. It uses Rust
bindings, chunked disk-backed DVZip/CPIO and explicit response limits, without
reqwest's pool or implicit TCP keepalive timeout. See the
[implementation and validation notes](docs/opendrop-sender-2026-09-17.md).
Real Mac/iPhone outgoing delivery still needs validation with this version.
