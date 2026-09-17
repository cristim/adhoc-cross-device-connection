#!/bin/sh
# Enable daemon diagnostics, restart ac-dc, and collect a concise health report.
set -u

UNIT=ac-dc-daemon.service

die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

case "${1:-}" in
  --disable)
    printf 'Disabling ac-dc debug logging...\n'
    pkexec /bin/sh -c 'rm -f /etc/systemd/system/ac-dc-daemon.service.d/debug.conf
      systemctl daemon-reload
      systemctl restart ac-dc-daemon.service'
    printf 'Debug logging disabled.\n'
    exit $?
    ;;
  "") ;;
  *) die "usage: $0 [--disable]" ;;
esac

command -v pkexec >/dev/null 2>&1 || die "pkexec is required"
command -v ac-dc >/dev/null 2>&1 || die "ac-dc is not installed"

printf 'Enabling ac-dc debug logging and restarting the daemon...\n'
pkexec /bin/sh -c 'mkdir -p /etc/systemd/system/ac-dc-daemon.service.d
  printf "%s\\n" "[Service]" "Environment=RUST_LOG=ac_dc=debug" > /etc/systemd/system/ac-dc-daemon.service.d/debug.conf
  systemctl daemon-reload
  systemctl restart ac-dc-daemon.service' || die "could not configure/restart $UNIT"

printf '\n== Identity ==\n'
id
printf 'Groups: '; id -nG

printf '\n== Daemon ==\n'
systemctl is-active "$UNIT" 2>&1 || true
systemctl --no-pager --full status "$UNIT" 2>&1 | sed -n '1,18p'

printf '\n== Control socket ==\n'
if [ -e /run/ac-dc/control.sock ]; then
  stat -c 'owner=%U group=%G mode=%a path=%n' /run/ac-dc/control.sock
else
  printf 'MISSING: /run/ac-dc/control.sock\n'
fi

printf '\n== CLI status ==\n'
RUST_LOG=ac_dc=debug ac-dc ctl status 2>&1 || true

printf '\n== Recipient discovery ==\n'
RUST_LOG=ac_dc=debug ac-dc ctl peers 2>&1 || true

printf '\n== AWDL health ==\n'
if command -v awdlctl >/dev/null 2>&1; then
  awdlctl status 2>&1 || true
else
  printf 'awdlctl not installed\n'
fi

printf '\n== Recent daemon journal ==\n'
journalctl -u "$UNIT" --since '2 minutes ago' --no-pager -o short-precise 2>&1 | tail -80

printf '\nDebug logging is enabled. Disable it later with:\n  %s --disable\n' "$0"
