# Adhoc Cross-Device Connection 0.1.0-0

Build locally with `makepkg -si` and install the resulting package alongside
brcmfmac-awdl-local >= 0.2.1. Enable the service, then launch `ac-dc ui` from
the terminal or app launcher:

    sudo systemctl enable --now ac-dc-daemon.service
    ac-dc ui
Choose Receive for 10 minutes, then approve incoming requests in the widget or CLI.
Choose Find recipients with the Apple device's Airdrop-compatible set to Everyone.
Select files, a folder, or an HTTP(S) link and send to the selected device.
Outgoing progress counts bytes sent; delivery is confirmed only after the reply.
Cancel stops the outgoing request. Closing the UI releases its radio window.

The receive button renews the radio window. Long transfers must finish before
that window expires; renew it when necessary. Bluetooth wake is enabled by
default in the UI to preserve the tested receive setup, and can be disabled.
Change the checkbox before starting a new receive window.

When UFW is enabled, allow only the AWDL receiver endpoint:

    sudo ufw allow in on awdl0 proto tcp from fe80::/10 to any port 8771 comment 'ac-dc Airdrop-compatible receiver on AWDL'

Undo with the matching `ufw delete allow ...` rule. This rule was applied on the
development machine before the user confirmed receiving; installation does not
change your firewall automatically.

Incoming archives are staged privately, then their top-level items are published
directly in the configured receive directory. Existing items are never replaced;
collisions use Finder-style names such as `photo 2.jpg` and `folder 2`. Paths and
entry types are checked; symbolic links are rejected.
Transfers are limited to 32 GiB decoded/encoded, with the older odc format's
per-file size limit. Empty-folder-only archives are not supported yet. Temporary
files require sufficient disk space, including compressed and decoded copies.

Everyone-mode identities are self-signed and not Apple account verification.
Clipboard research commands remain experimental: Airdrop-compatible success does not imply
Universal Clipboard compatibility. See docs/owl-opendrop-localsend-research.md.
