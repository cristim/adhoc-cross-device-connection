pkgname=adhoc-cross-device-connection
pkgver=0.1.0
pkgrel=0
pkgdesc='Rust Airdrop-compatible desktop UI and Apple clipboard research tools'
arch=('aarch64')
license=('MIT')
depends=('brcmfmac-awdl-local>=0.2.2' 'dbus' 'openssl' 'wl-clipboard' 'libnotify' 'libxcb' 'libxkbcommon' 'libxkbcommon-x11' 'wayland' 'libglvnd' 'vulkan-icd-loader' 'fontconfig' 'freetype2' 'ttf-font')
makedepends=('rust' 'pkgconf')
options=('!strip' '!debug')
build() { cd "$startdir"; cargo build --release --locked --offline; }
package() {
  install -Dm755 "$startdir/target/release/ac-dc" "$pkgdir/usr/bin/ac-dc"
  install -Dm755 "$startdir/scripts/ac-dc-debug.sh" "$pkgdir/usr/bin/ac-dc-debug"
  install -Dm644 "$startdir/packaging/org.adhoccrossdeviceconnection.desktop" "$pkgdir/usr/share/applications/org.adhoccrossdeviceconnection.desktop"
  install -Dm644 "$startdir/packaging/ac-dc-receive.service" "$pkgdir/usr/lib/systemd/user/ac-dc-receive.service"
  install -Dm644 "$startdir/packaging/ac-dc-daemon.service" "$pkgdir/usr/lib/systemd/system/ac-dc-daemon.service"
  install -Dm644 "$startdir/packaging/INSTALL.md" "$pkgdir/usr/share/doc/adhoc-cross-device-connection/INSTALL.md"
  install -Dm644 "$startdir/LICENSE" "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
