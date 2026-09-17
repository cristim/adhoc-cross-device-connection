# ac-dc

Apple Continuity and Airdrop-compatible file-transfer tools for Linux, implemented in Rust.

- Explore Universal Clipboard BLE announcements and companion-link traffic using
  keys exported from your own macOS account.
- Send and receive Airdrop-compatible files/links through the Omarchy systray
  widget, native Rust GTK UI, or CLI.
- Inspect prerequisites and advertise Apple BLE TLVs for controlled experiments.

## Project status

The current release focuses on a usable Linux desktop experience. The restricted
Rust daemon manages the radio session, while the Omarchy widget provides
recipient discovery, file/link sending, receiving, a ten-minute countdown, and
configurable name and destination. The transfer/archive protocol passes local
Rust interoperability tests; real-device compatibility remains dependent on
Apple firmware and Everyone-mode settings.

Universal Clipboard support is a future goal, not a completed feature. BLE
announcement parsing, OPACK, companion-link framing, and pasteboard plumbing
are useful research foundations, but current-device authentication, keyed
archives, request payloads, and end-to-end pasteboard exchange still need
reverse engineering and validation against Apple devices.

We welcome reverse-engineering help: packet captures from devices you own,
wire-format analysis, protocol comparisons, and reproducible tests are especially
valuable. Please do not share private keys or captures containing personal data.

### Current hardware limitation

On the current BCM4378/BCM4387 test machine, AWDL management traffic is
working: nearby Apple devices advertise `_airdrop._tcp` and their endpoints are
decoded correctly. The remaining failure is the unicast data path: IPv6
neighbors become `FAILED`, TCP probes time out, and the firmware reports rising
AWDL `txdrop` counters with no corresponding data RX. A graceful DKMS driver
reload does not currently resolve this. As a result, recipient discovery may
return zero usable recipients and real transfers are not presently reliable.
This is being investigated in the sibling [brcmfmac-awdl repository](https://github.com/cristim/brcmfmac-awdl);
the likely area is the patched driver/firmware data path rather than the GUI,
Unix socket permissions, or mDNS advertisement parsing.

## Separate driver repository

The patches and Linux radio tools now live in **`../brcmfmac-awdl/`**, normally
`~/src/brcmfmac-awdl`. That repository retains the driver history and the newer
patches from `feat/awdl-4378-data-path`. It provides Rust `awdlctl`; it owns
interface creation, firmware iovars, peer registration, channel schedules, and
bounded radio/band management. There is no Rust path dependency between repos.

`ac-dc` runs as your desktop user. Configure AWDL with the driver tools separately;
it never installs/reloads a driver or silently changes your Wi-Fi connection.
The driver package includes the Omdrop-compatible AWDL action-frame patches for
BCM4387-style firmware as well as the BCM4378 work maintained in the companion
repository. Hardware and firmware combinations still require validation.

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
ac-dc ctl status
```

The widget and the CLI talk to the service through its local Unix socket; no
network control port is exposed. The socket is readable by the `wheel` group;
log out and back in after installation if your account was just added to that
group. See [packaging/INSTALL.md](packaging/INSTALL.md) for first-transfer
guidance.

Tests use temporary data and loopback TCP/TLS. They never access real Apple keys,
change the radio, or change the desktop clipboard.

## Universal Clipboard research commands

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

The Omarchy systray widget is the recommended lightweight interface. Opening it
automatically discovers nearby recipients and refreshes them every second. It
provides Receive/Stop, a ten-minute receive countdown, recipient selection,
file selection, link sending, and Settings for the display name and receive
directory. The full GTK window remains available through `ac-dc ui` when needed.

Set the receiving Apple device to **Everyone for 10 Minutes**. Open the widget,
select the intended recipient, select files or enter an HTTP(S) link, and send.
Nearby names are discovery hints, not authenticated Apple identities.
Everyone-mode TLS uses self-signed certificates; Contacts Only is not implemented.

CLI equivalents (the root daemon owns the radio window; no `pkexec` is needed
for normal discovery or transfer commands):

```sh
ac-dc ctl peers
ac-dc ctl send --host 'fe80::PEER%awdl0' --port 8770 \
  --name Linux --file /path/to/file
# For a link, replace the filename with --url https://example.com/
```

### Diagnostic logging

Normal commands emit compact JSON responses on stdout; diagnostic logs go to
stderr so the widget and scripts remain machine-readable. Increase logging for
one invocation with `-v` (debug) or `-vv` (trace):

The installed `ac-dc-debug` helper automates the service setup and prints a
health report in one pass. It asks for authentication once through `pkexec`:

```sh
ac-dc-debug
```

It checks the daemon, socket ownership, CLI status, recipient discovery, AWDL,
and the last two minutes of daemon logs. Disable the persistent debug override
later with `ac-dc-debug --disable`.

```sh
ac-dc -v ctl status
ac-dc -vv ctl receive --name Omarchy --directory ~/Downloads/Adhoc
```

The same levels can be enabled for the system daemon without changing its
protocol output by setting `RUST_LOG` in the service environment. For a live
diagnostic session, use:

```sh
sudo systemctl edit ac-dc-daemon.service
# Add these lines in the editor:
# [Service]
# Environment=RUST_LOG=ac_dc=debug
sudo systemctl restart ac-dc-daemon.service
journalctl -u ac-dc-daemon.service -f
```

Trace logs include control requests, peer candidates, connection/session
identifiers, HTTP endpoints and byte counts, but never private keys, TLS
credentials, or file contents. Use `RUST_LOG=ac_dc=trace` only while actively
troubleshooting because it can be verbose.

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
These commands are for protocol research and should not be presented as a
finished clipboard workflow.

## 0.1.0-0 release notes

See [installation and operating limits](packaging/INSTALL.md) and
[OWL, OpenDrop and LocalSend findings](docs/owl-opendrop-localsend-research.md).
The Rust UI and Omarchy widget add outgoing progress/cancellation, folder
selection and a renewable radio countdown. Archives stream through temporary files; completed top-level
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
