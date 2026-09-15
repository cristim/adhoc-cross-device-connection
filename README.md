# ac-dc

Apple Continuity and clipboard tools for Linux, implemented in Rust.

- Observe and decrypt Universal Clipboard BLE announcements using keys exported
  from your own macOS account.
- Pull a pasteboard through companion-link, with mandatory peer signature
  verification, scoped IPv6 discovery, and Wayland clipboard output.
- Run an explicit, bounded **AirDrop receive** session for shared links and files.
- Inspect prerequisites and advertise Apple BLE TLVs for controlled experiments.

**Compatibility is experimental.** Companion-link framing and request fields
still need validation against Apple devices. The AirDrop receiver is tested
against a local TLS client, not an iPhone/Mac. A mock round-trip does not prove
Apple interoperability. The BCM4378 AWDL transport also remains unvalidated.

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

## AirDrop receive

Prepare an AWDL window and peer registration using `awdlctl`, then run as your
normal user:

```sh
ac-dc receive --directory ~/Downloads/AirDrop \
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

AirDrop's AWDL management-frame publication/wake requirements can differ from
mDNS publication. This receiver does not yet generate the firmware PSF/service
templates used by Omdrop. A missing AirDrop tile is therefore not sufficient to
conclude that its HTTP receiver or the radio is broken.

## BLE advertising experiments

```sh
ac-dc advertise --airdrop-wake --seconds 30
ac-dc advertise --data HEX_APPLE_TLVS --seconds 10 --interval-ms 100
```

Registration and cancellation use BlueZ; no raw HCI commands compete with
bluetoothd. `--airdrop-wake` emits an AirDrop type-5 advert, not a Universal
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
