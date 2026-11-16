#!/usr/bin/env bash
# End-to-end check for app-based split tunneling on Linux.
#
#   sudo scripts/validate-app-split.sh
#
# Needs root (it creates a cgroup, firewall rules and a routing table) and
# stands a dummy device in for the VPN tunnel, so it can run without a session:
#
#   process in cgroup -> iptables MARK -> ip rule -> table 100 -> dummy device
#
# The decisive measurement is at POSTROUTING: a counter for "carries the mark
# and leaves through the dummy" versus "carries the mark and leaves through the
# physical interface". That separates "the mark was applied" from "the mark
# actually rerouted the packet" — the difference between the xtables MARK target
# and nftables' `meta mark set`, which runs after the route lookup.
#
# The probe runs as the invoking user (not root) so the helper's ownership guard
# accepts it, exactly as in real use.
#
# Build the helper first:  cargo build -p nimbus-helper --manifest-path src-tauri/Cargo.toml
set -uo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
HELPER=${NIMBUS_HELPER:-$HERE/../src-tauri/target/debug/nimbus-helper}
PROBE_SRC=$HERE/app-split-probe.c
WORK=${TMPDIR:-/tmp}/nimbus-split-check
PROBE=$WORK/nimbus-probe
LOG=$WORK/check.log
DEV=nimbus-test0
PHYS=$(ip route show default | awk '{print $5}' | head -1)
USER_UID=${SUDO_UID:-$(id -u)}
USER_NAME=$(id -nu "$USER_UID")

mkdir -p "$WORK"
: >"$LOG"
say() { echo "$*" | tee -a "$LOG"; }
as_user() { runuser -u "$USER_NAME" -- "$@"; }
pass() { say "PASS $*"; }
fail() { say "FAIL $*"; }

clean_policy() {
  iptables -t mangle -D OUTPUT -m cgroup --path "nimbus.apps" -j MARK --set-mark 0x4e4d 2>/dev/null
  ip6tables -t mangle -D OUTPUT -m cgroup --path "nimbus.apps" -j DROP 2>/dev/null
  ip rule del fwmark 0x4e4d lookup 100 pref 1000 2>/dev/null
  ip route flush table 100 2>/dev/null
}

if [ "$(id -u)" -ne 0 ]; then
  echo "run as root: sudo $0" >&2
  exit 2
fi
if [ ! -x "$HELPER" ]; then
  echo "helper not built: $HELPER" >&2
  echo "  cargo build -p nimbus-helper --manifest-path src-tauri/Cargo.toml" >&2
  exit 2
fi

say "user under test: $USER_NAME ($USER_UID), physical interface: $PHYS"
say "helper: $HELPER"
say "log:    $LOG"

say "== pre-clean"
pkill -f "$PROBE" 2>/dev/null
ip link del "$DEV" 2>/dev/null
clean_policy

cc -O2 -o "$PROBE" "$PROBE_SRC" || { say "could not build the probe"; exit 1; }
chmod 755 "$WORK"

say "== dummy device"
ip link add "$DEV" type dummy
# The dummy's own IPv6 link traffic would pollute any packet count on it.
sysctl -q -w "net.ipv6.conf.$DEV.disable_ipv6=1"
ip link set "$DEV" up
ip addr add 10.99.0.1/24 dev "$DEV"
ip -br addr show "$DEV" | tee -a "$LOG"

# ── phase 1: policy installed ─────────────────────────────────────────────
say "== phase 1: probe as $USER_NAME with the policy active"
as_user "$PROBE" >"$WORK/probe1.log" 2>&1 &
sleep 1
PROBE_PID=$(pgrep -u "$USER_UID" -f "$PROBE" | head -1)

"$HELPER" enforce --apps nimbus-probe --tunnel "$DEV" >"$WORK/enforce.log" 2>&1 &
ENFORCE_PID=$!
sleep 2

say "-- status:"
"$HELPER" status | tee -a "$LOG"
if grep -qx "$PROBE_PID" /sys/fs/cgroup/nimbus.apps/cgroup.procs 2>/dev/null; then
  pass "probe adopted into cgroup"
else
  fail "probe not adopted"
fi
if iptables -t mangle -C OUTPUT -m cgroup --path "nimbus.apps" -j MARK --set-mark 0x4e4d 2>/dev/null; then
  pass "mark rule installed"
else
  fail "mark rule missing"
fi

say "== postrouting counters"
nft add table inet nimbus_check
nft add chain inet nimbus_check post '{ type filter hook postrouting priority 100; }'
nft add rule inet nimbus_check post meta mark 0x4e4d oifname "$DEV" counter
nft add rule inet nimbus_check post meta mark 0x4e4d oifname "$PHYS" counter

sleep 6
say "-- iptables mangle OUTPUT:"
iptables -t mangle -L OUTPUT -v -x -n 2>/dev/null | tee -a "$LOG"
say "-- postrouting:"
nft list chain inet nimbus_check post | tee -a "$LOG"
say "-- probe output:"
cat "$WORK/probe1.log" | tee -a "$LOG"

VIA_TUNNEL=$(nft list chain inet nimbus_check post | grep "oifname \"$DEV\"" | grep -o 'packets [0-9]*' | awk '{print $2}')
VIA_PHYS=$(nft list chain inet nimbus_check post | grep "oifname \"$PHYS\"" | grep -o 'packets [0-9]*' | awk '{print $2}')
say "marked via tunnel: ${VIA_TUNNEL:-0}   marked via physical: ${VIA_PHYS:-0}"
if [ "${VIA_TUNNEL:-0}" -ge 3 ]; then
  pass "${VIA_TUNNEL} marked datagram(s) left through the tunnel device"
else
  fail "only ${VIA_TUNNEL:-0} marked datagram(s) left through the tunnel device"
fi
if [ "${VIA_PHYS:-0}" -eq 0 ]; then
  pass "no marked datagram leaked out of the physical interface"
else
  fail "${VIA_PHYS} marked datagram(s) leaked out of the physical interface"
fi

say "== stop helper"
kill -TERM "$ENFORCE_PID" 2>/dev/null
wait "$ENFORCE_PID" 2>/dev/null
sleep 1

say "== teardown verification"
"$HELPER" status | tee -a "$LOG"
if iptables -t mangle -C OUTPUT -m cgroup --path "nimbus.apps" -j MARK --set-mark 0x4e4d 2>/dev/null; then
  fail "mark rule still present"; else pass "mark rule removed"; fi
if ip rule show | grep -q '0x4e4d'; then
  fail "policy rule still present"; else pass "policy rule removed"; fi
if ip route show table 100 | grep -q .; then
  fail "table 100 still has routes"; else pass "table 100 empty"; fi
if [ -d /sys/fs/cgroup/nimbus.apps ]; then
  fail "cgroup still present"; else pass "cgroup removed"; fi

kill "$PROBE_PID" 2>/dev/null
sleep 0.3
nft delete table inet nimbus_check 2>/dev/null

# ── phase 2: negative control ─────────────────────────────────────────────
say "== phase 2: same probe with no policy (must not be marked at all)"
nft add table inet nimbus_check
nft add chain inet nimbus_check post '{ type filter hook postrouting priority 100; }'
nft add rule inet nimbus_check post meta mark 0x4e4d oifname "$DEV" counter

as_user "$PROBE" >"$WORK/probe2.log" 2>&1 &
sleep 8
cat "$WORK/probe2.log" | tee -a "$LOG"
nft list chain inet nimbus_check post | tee -a "$LOG"
CONTROL=$(nft list chain inet nimbus_check post | grep -o 'packets [0-9]*' | awk '{print $2}')
if [ "${CONTROL:-0}" -eq 0 ]; then
  pass "unmarked probe traffic never entered the tunnel path"
else
  fail "${CONTROL} datagram(s) were marked without a policy"
fi

say "== cleanup"
nft delete table inet nimbus_check 2>/dev/null
ip link del "$DEV" 2>/dev/null
pkill -f "$PROBE" 2>/dev/null
say "== done"
