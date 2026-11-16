#!/usr/bin/env bash
# Install (or remove) the Nimbus split tunneling helper.
#
#   sudo scripts/install-helper.sh
#   sudo scripts/install-helper.sh --uninstall
#
# Builds the release binary, installs it to /usr/libexec/nimbus/nimbus-helper
# (root-owned, which is what makes it safe for the daemon to run as root), then
# installs and starts the systemd unit.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
UNIT=nimbus-helper.service
UNIT_PATH=/etc/systemd/system/$UNIT
LIBDIR=/usr/libexec/nimbus
BINARY=$LIBDIR/nimbus-helper

if [ "$(id -u)" -ne 0 ]; then
  echo "run as root: sudo $0" >&2
  exit 2
fi

if [ "${1:-}" = "--uninstall" ]; then
  systemctl disable --now "$UNIT" 2>/dev/null || true
  rm -f "$UNIT_PATH"
  systemctl daemon-reload
  rm -rf "$LIBDIR"
  echo "removed $UNIT_PATH and $LIBDIR"
  exit 0
fi

# Build as the invoking user: cargo would otherwise litter root-owned files in
# the checkout, and the target directory is owned by them.
if [ -n "${SUDO_USER:-}" ] && [ "$SUDO_USER" != "root" ]; then
  BUILD=(runuser -u "$SUDO_USER" -- cargo build --release -p nimbus-helper --manifest-path "$ROOT/src-tauri/Cargo.toml")
else
  BUILD=(cargo build --release -p nimbus-helper --manifest-path "$ROOT/src-tauri/Cargo.toml")
fi
echo "building…"
"${BUILD[@]}"

BUILT=$ROOT/src-tauri/target/release/nimbus-helper
if [ ! -x "$BUILT" ]; then
  echo "build did not produce $BUILT" >&2
  exit 1
fi

install -d -o root -g root -m 0755 "$LIBDIR"
install -o root -g root -m 0755 "$BUILT" "$BINARY"
install -o root -g root -m 0644 "$ROOT/src-tauri/helper/$UNIT" "$UNIT_PATH"

systemctl daemon-reload
systemctl enable "$UNIT"
# `enable --now` does not restart an already-running unit, so an upgrade would
# keep executing the previous binary until the next reboot.
systemctl restart "$UNIT"
sleep 1

echo
systemctl --no-pager --lines=0 status "$UNIT" || true
echo
echo "installed $BINARY"
echo "socket: /run/nimbus/helper.sock"
echo
echo "check:  sudo $BINARY status"
