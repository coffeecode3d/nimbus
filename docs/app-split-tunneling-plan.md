# App-based split tunneling — design and feasibility

Status: proposal, partly decided. Not implemented. This supersedes the
app-based section of `docs/split-tunneling-plan.md`.

Decisions so far:

- First milestone is the **Linux root helper** in **include mode** ("only these
  apps use the VPN"), because that is the actual ask.
- Eddie `network.ipv4.mode=out` is **validated on Linux** for WireGuard and
  OpenVPN (9/9 checks, see §9.1), with two prerequisites: `dns.check=false` and
  `dns.mode=none`.
- Windows: **no free path exists**, so 0.2.0 hides the feature there. Per-app
  include needs a WFP kernel driver, and Windows only loads drivers signed by
  Microsoft (EV certificate, verified business, ~$300-500/year). Test-signing
  and disabling Secure Boot are not shippable. Mullvad's driver cannot be reused
  for include mode: it is explicitly exclude-only ("redirect socket binds away
  from the tunnel interface"), and it demands a private IOCTL protocol plus its
  own WFP filters. The next Windows step is an **exclude** feature, which also
  needs a driver — the only free route is reusing Mullvad's signed GPL driver,
  at the cost of depending on their binary and untested IOCTL coupling.

## 1. Goal

Let the user choose one or more applications:

- **Include mode:** only the selected apps use the VPN; everything else uses
  the normal connection.
- **Exclude mode:** everything uses the VPN except the selected apps.

The picker should list running apps, grouped by executable/desktop entry, and
allow selecting one or several. "Apply the VPN to Helium" is include mode.

## 2. Why this is not a UI-only feature

Nimbus is a UI over Eddie. Eddie decides routing globally: it installs a
default route through the tunnel and has no process awareness.
`routes.custom` matches destinations, not processes. To steer traffic by
process, something must classify packets/sockets and apply a policy route or
firewall mark.

- **Linux:** cgroup v2 + nftables/iptables marks + policy routing. Root is
  required to install the marks and rules.
- **Windows:** WFP callouts in kernel mode. A signed driver is required to
  steer sockets; user-mode WFP filters can permit/block but not choose a
  different route or interface per app.

The app list UI is easy. Enforcement is the hard part.

## 3. Semantics and difficulty

| Mode | Meaning | Difficulty |
|---|---|---|
| Exclude | Selected apps bypass the VPN | Moderate: Eddie's default route stays in; mark selected apps to a table with the physical default. |
| Include | Only selected apps use the VPN | Hard: the default route must be outside; selected traffic must be steered into the tunnel; DNS must be split. |

Inclusion is what "apply the VPN to Helium" means, and it is the harder mode.

## 4. Linux design (chosen first milestone)

Include mode: only the selected apps use the VPN. Everything else keeps the
physical interface, and Eddie owns nothing but the tunnel.

### 4.1 Prerequisites

- Eddie connects with `network.ipv4.mode=out` (`RouteAllIPv4 = false`): the
  tunnel device is configured, but Eddie installs no default route through it.
  `scripts/validate-out-mode.mjs` exists to prove exactly this.
- `network.ipv6.mode=out` in the prototype, plus an IPv6 drop for the selected
  cgroup, so an included app cannot leak over IPv6.
- **`dns.check=false` is mandatory.** Eddie's post-connect check resolves a
  hash domain through the tunnel resolver and asks the exit whether it saw the
  query. In `out` mode there is no route to that resolver, so the check fails,
  the session is reset, and the engine loops on "Checking DNS failed". The
  option is the same one Eddie exposes as Settings → DNS check.
- `dns.mode=none`, so Eddie leaves the system resolver alone and the helper can
  supply a tunnel-bound resolver to the selected apps only.
- Network lock stays off. Netlock and inclusion are opposites: netlock blocks
  traffic outside the tunnel, which is exactly what non-selected apps need.

Eddie still adds two routes of its own in `out` mode, and both matter:

| Route | Purpose |
|---|---|
| `<exit IP> dev Eddie` | the session **exit** IP, through the tunnel |
| `<entry IP> via <lan gateway> dev enp42s0` | the server **entry** IP, outside the tunnel |

The exit route is why Eddie's own route check still passes in `out` mode. The
**DNS server IP is not routed**, which is both why `dns.check` fails and why the
helper cannot simply point included apps at the tunnel resolver: the forwarder
has to bind the tunnel device to reach it.

### 4.2 Classification: cgroup v2

- The helper creates `/sys/fs/cgroup/nimbus.apps` (unified hierarchy; this host
  reports `cgroup2fs`).
- Selected processes are moved in by writing their PID to `cgroup.procs`.
  Children inherit the cgroup, so only the top-level process must be caught.
- **One PID per `write()`.** `cgroup.procs` parses a single number, so a buffer
  holding several PIDs fails with `EINVAL` and moves nothing (measured: 27 PIDs
  in one write failed; the same 27 one at a time all succeeded). This only shows
  up once an app has more than one process, which is why a single-process smoke
  test missed it; `scripts/validate-app-split.sh` now adopts three.
- Both firewall rules match the cgroup by path (`-m cgroup --path nimbus.apps`),
  which covers the whole subtree and does not depend on an inode that changes
  when the directory is re-created.
- **The directory must exist before the rules are installed**: the cgroup match
  is resolved when the rule is parsed, and `-m cgroup --path` fails outright for
  a path that is not there yet.
- A 500 ms `/proc` watcher re-adds processes whose `exe` basename matches a
  selected app, so relaunching Helium keeps it inside the tunnel. Sockets that
  already existed when the PID moved are not re-steered; the documented remedy
  is to relaunch the app.
- Only processes owned by the invoking user are adopted (`SUDO_UID` /
  `PKEXEC_UID`), so a user can never steer a root daemon into their tunnel.

### 4.3 Steering: mark + policy route

- `iptables -t mangle -A OUTPUT -m cgroup --path nimbus.apps -j MARK
  --set-mark 0x4e4d`.
- `ip rule add fwmark 0x4e4d lookup 100 pref 1000`.
- Table 100 holds the tunnel's own routes plus `default dev <tun>`. Only marked
  packets consult table 100, so the main table keeps the physical default and
  non-selected apps are unaffected.
- The helper reads the tunnel device and its routes from Eddie's live session
  instead of guessing names (`Eddie`, `tun0`, …).

**Why iptables and not nftables.** This is the single most important finding of
the prototype. For locally generated traffic the IPv4 route lookup happens
*before* the netfilter OUTPUT hook, so setting `meta mark` with nftables there
attaches the mark but does **not** reroute the packet. Measured on this host
(kernel 7.3, nft 1.1.7, iptables 1.8.13/nf_tables):

```
product rule counter            marked packets 3
postrouting, mark + tunnel dev  packets 3      ← with iptables MARK
postrouting, mark + physical    packets 0
...
with nft `meta mark set`:
postrouting, mark + tunnel dev  packets 0
postrouting, mark + physical    packets 3      ← packets leaked
```

**The tunnel must not route its own transport.** WireGuard's encapsulated
packets carry the inner packet's mark, so the route to the tunnel's own
endpoint (`198.44.133.70`, added by Eddie via the physical gateway) matched
table 100's `default dev Eddie` and the tunnel fed itself:

```
Eddie TX while idle: 192,358 packets in 3 seconds (64k pps), RX: 0
```

Nothing ever reached the exit, so every connection hung with no reply. Copying
the endpoint's host route into table 100 — `198.44.133.70 via <lan gateway> dev
<physical>` — drops that to **0 packets per 3 seconds** and traffic flows.
`plan::bypass_routes` copies every host route that does not point at the tunnel.

**The source address is the second half of it.** A local socket chooses its
source when it resolves the route, which happens *before* any packet exists and
therefore before the OUTPUT hook can mark it. The reroute to the tunnel keeps
that choice, so an included app sends packets that claim to come from the LAN:

```
OUT=nimbus-src0  SRC=192.168.6.124  DST=203.0.113.5  MARK=0x5151
conntrack: src=192.168.6.124 ... [UNREPLIED] src=203.0.113.5 dst=192.168.6.124
```

The AirVPN exit drops those packets — the app shows "connected" but nothing
loads. Rewriting the source on the way out fixes it:

```
iptables -t nat -A POSTROUTING -m mark --mark 0x4e4d -j MASQUERADE
```

and conntrack then expects the reply at the tunnel address (`dst=10.99.0.1`).
No `-o` device is given, so teardown does not need to know the tunnel name.

The xtables `MARK` target has the behaviour the design needs: `iptable_mangle`
calls `ip_route_me_harder()` when a rule changes `skb->mark`, so the packet is
rerouted through table 100 before transmission. `nimbus-helper` therefore uses
`iptables` for the mark and `ip6tables` for the IPv6 drop, and its counters are
read back with `iptables -t mangle -L OUTPUT -v -x -n`.


### 4.4 DNS

This is the part that makes or breaks the feature.

Implemented without a proxy or a forwarder:

```
iptables -t nat -A OUTPUT -m cgroup --path nimbus.apps -p udp --dport 53 \
  -j DNAT --to-destination 1.1.1.1:53
```

and the same for TCP. An included app asks the loopback resolver, which answers
over the *physical* link and leaks every name it looks up. Rewriting the
destination to a public resolver fixes that almost for free: the packet already
carries the mark when the NAT chain runs, so the reroute lands in table 100 and
the query travels through the tunnel like any other traffic.

Details and trade-offs:

- Both transports are covered. A resolver retries over TCP when a reply is
  truncated, and that path would otherwise leak.
- The reply is un-NATed twice (DNAT back to the loopback resolver, SNAT back to
  the app), so the app sees an answer from the address it asked.
- AirVPN's own resolvers live inside the tunnel, but Eddie does not expose them
  over jsoninout, so `1.1.1.1` is used. The query leaves through the VPN, so the
  ISP cannot see it; the exit can, as it could with any plaintext DNS.
- Local-only names (`.local`, the physical network's search domain) stop
  resolving for included apps. That is the cost of not leaking, and the UI says
  so.
- DoH/DoT is untouched: those connections are already inside the tunnel.
- Non-selected apps keep the original resolver.

### 4.5 Lifecycle

- Connect: Eddie reports connected → Nimbus starts the helper for the session
  and passes the tunnel device → the helper installs cgroup, routes, policy
  rule, mark rule, and adopts the selected processes.
- Disconnect: Nimbus stops the helper *before* Eddie tears the tunnel down, so
  no packet is ever marked into a table whose device has gone. The helper's
  teardown order is: mark rule → table 100 → policy rule → cgroup.
- Crash recovery: the helper is started with `--parent-death`, so Linux signals
  it when Nimbus exits, and it also watches stdin for EOF. A killed UI
  therefore cannot leave a mark or route behind. `begin` additionally starts by
  clearing any state a previous run left, so a `SIGKILL`ed helper is repaired on
  the next connect.
- Reconnect / route change: `begin` is idempotent and re-reads the tunnel.

### 4.6 Security

The helper is a **per-session root process**, not a long-lived daemon. Nimbus
starts it through polkit (`pkexec`), the same way Eddie starts its own elevated
helper, so authentication stays with polkit and no socket or daemon has to be
installed. It runs only while a session is up.

- No shell, ever: every command is a program plus an argument vector generated
  from validated values. App ids are executable basenames from `/proc`, the
  tunnel is a device name that must exist in `/sys/class/net`, and route
  destinations are checked to be plain address prefixes.
- Only processes owned by the invoking user (`SUDO_UID` / `PKEXEC_UID`) are
  adopted, so the helper cannot be tricked into steering a root daemon into a
  user's tunnel. `--allow-any-uid` exists for a deliberate root-only session.
- The UI never runs as root and never passes arbitrary paths or firewall
  arguments; it passes app ids and a tunnel device.
- If a long-lived daemon is wanted later, this helper's command surface is
  already structured for it (`begin`/`end`/`status` semantics, no free-form
  input).

- The UI never runs as root and never touches the privileged state.

### 4.7 Exclude mode

Same plumbing, opposite policy, and no DNS work:

- Eddie stays in `in` mode, so the default route is the tunnel.
- The marked cgroup goes to a table whose default is the physical interface.
- Excluded apps still use Eddie's resolver, so a hostname can resolve to an
  address that is not excluded. Document that limitation.

## 5. Windows design (chosen: exclude mode on Mullvad's driver)

Include-mode parity on Windows is not shippable for free: it needs a WFP kernel
driver, and Windows only loads drivers signed by Microsoft (EV certificate,
verified business, and an HSM/token for every signing session). Test-signing and
disabling Secure Boot are not options we can ask of users.

So Windows gets **exclude mode** instead, which is the direction Mullvad's
already-signed GPL-3.0 driver implements: Eddie stays in its normal `in` mode and
the selected apps are pushed out of the tunnel.

Crate `src-tauri/helper-win` (`nimbus-split-win`):

- `proto` — the driver's wire format, transcribed from `defs/*.h`: IOCTL codes,
  `ST_IP_ADDRESSES`, the configuration and process-discovery blocks with
  UTF-16 image paths, event decoding, and the documented state machine. Pure
  encoding, unit-tested on any platform (9 tests).
- `agent` — the decision layer: selected Win32 paths → device paths the driver
  matches on, plus the process tree and addresses to register. The OS is behind
  a `System` trait, so the flow is tested with a fake (5 tests).
- `windows_system` — the `System` implementation: `QueryDosDeviceW` for device
  paths, `CreateToolhelp32Snapshot` + `QueryFullProcessImageNameW` for the tree
  (opening with `PROCESS_QUERY_LIMITED_INFORMATION`, the least right that
  works), and `GetAdaptersAddresses` for the tunnel and physical addresses.
  Anything it cannot identify is left as zero, which the driver reads as "that
  family is unavailable" rather than a guess.
- `driver` — opens `\\.\MULLVADSPLITTUNNEL`, walks
  `STARTED → INITIALIZED → READY → ENGAGED`, drains events. Type-checked
  against `x86_64-pc-windows-gnu`; not yet exercised on Windows.

The wire format was checked against the upstream headers rather than only
against our own unit tests, because a wrong size would fail on the machine and
nowhere else:

- `ST_IP_ADDRESSES` is four unpadded addresses — `IN_ADDR`, `IN_ADDR`,
  `IN6_ADDR`, `IN6_ADDR` — 40 bytes in that order.
- `ST_CONFIGURATION_ENTRY` is 16 bytes and `ST_PROCESS_DISCOVERY_ENTRY` is 32 on
  x64, both with six bytes of tail padding after the `USHORT` length; the
  offsets in those entries are relative to the string blob that follows the
  whole entry array, not to the start of the buffer.
- `ST_EVENT_HEADER` has a four-byte `EventId`, four bytes of padding, then a
  `SIZE_T` size; the payload starts at byte 16. The error event ids really are
  `0x80000000 + n`, so the field is unsigned.
- The driver rejects a buffer whose `TotalLength` is not exactly the buffer
  length, and requires the IP address buffer to be exactly 40 bytes.
- `IOCTL_ST_REGISTER_IP_ADDRESSES` is only accepted in the `READY` state, so the
  order is fixed: initialize, register the process tree, register addresses,
  then set the configuration.
- `IOCTL_ST_GET_STATE` requires an 8-byte output buffer even though the state
  itself is a 32-bit value.

The driver's own "valid address" test is merely "not all zero", so the agent can
still be exercised without a live tunnel by passing explicit addresses.

Remaining before it ships:

1. Vendor the signed `mullvad-split-tunnel.sys` (from their installer) and
   install it from NSIS, with the GPL-3.0 attribution already in `NOTICE`.
2. Wrap the agent in a Windows service with a named-pipe API, so the
   unprivileged UI can send the exclude list.
3. UI: an exclude-mode toggle on the Apps page for Windows.
4. Validate the state machine against the real driver in a Windows VM.

### 5.4 Test VM

Testing this from Linux needs a real Windows guest, because the driver only
loads on Windows and only when it is signed. The development laptop runs:

    ~/VMs/win11/run.sh cd    # first boot: install from the ISO
    ~/VMs/win11/run.sh disk  # afterwards

QEMU/KVM with OVMF and an answer file in the root of a remastered Windows 11
Enterprise Evaluation ISO. The unattended parts are the hardware-gate bypass,
the local administrator (`nimbus`) and OpenSSH, which is enabled on first logon
so the guest is driven over `ssh -p 2222` instead of the GUI. One keystroke is
still needed: the partition page, where accepting "Disk 0 Unallocated Space"
lets Setup create the partitions. VNC on `127.0.0.1:5901` and a QEMU monitor
socket exist for the moments a screenshot is the only way to see what is
happening.

Installing Windows unattended on top of QEMU cost a day of debugging. The
findings, in the order they bit:

- **`/tmp` is tmpfs.** An 8 GB ISO downloaded to `/tmp` fills RAM and dies at
  exactly 8 GB with "failure writing output to destination". Download to disk.
- **`swtpm` may be unloadable** (built against a newer OpenSSL than the host
  has). The answer file therefore carries the `Setup\LabConfig` bypasses and the
  TPM device is optional.
- **An explicit `ich9-ahci` leaves nothing on the default bus.** Any drive given
  without `if=none`/`-device` ends up in `/machine/unattached` and is invisible
  to the firmware — including the CD and the answer file. Attach every drive to
  a stated port.
- **Stale NVRAM hides the DVD.** Repeated failed boots fill the variable store;
  deleting `vars.fd` restores the `UEFI QEMU DVD-ROM` entry.
- **The "press any key to boot from CD" prompt expires in about five seconds**,
  so keys have to be sent through the firmware boot, not once at start.
- **`autounattend.xml` is only honoured from the root of the installation
  media.** A FAT disk, a USB stick and a floppy were all ignored (and q35 has no
  floppy controller anyway).
- **The hardware gate is evaluated in `windowsPE`, not `specialize`.** Writing
  `BypassTPMCheck` from the `specialize` pass is too late — Setup has already
  refused. It must be a `RunSynchronous` command in the `windowsPE` pass.
- **Windows media is a UDF/ISO9660 hybrid**, and `install.wim` is over 4 GiB.
  This `xorriso` has no UDF writer, so the rebuild is plain ISO9660 and the image
  is split with `wimlib-imagex split install.wim install.swm 3800` to keep every
  file under 4 GiB — the same trick Rufus uses for FAT32 sticks, which Setup
  reads transparently.
- **A bad `<DiskConfiguration>` fails twice, far apart.** Setup rejects the block
  early with `UnattendProcessDiskConfigInfo: An internal error occurred while
  parsing unattend disk configuration` (`0x8007000D`), quietly falls back to the
  interactive partition page, and then fails *fatally* at PreFinalize in
  `CFixUnattendDrivePaths` — after the image has been applied, which is why it
  presents as "installation has failed" at around 55%. The fix was to delete the
  block entirely: leaving the disk unallocated means one Enter on the partition
  page and Setup lays out EFI/MSR/Primary itself, with the rest of the answer
  file still applying.
- **Read `setuperr.log` off the disk image rather than guessing from
  screenshots.** The target volume keeps
  `\$Windows.~BT\Sources\Panther\setuperr.log`, and it can be read by attaching
  the qcow2 with `qemu-nbd -c /dev/nbd0 --read-only` and mounting the NTFS
  partition read-only. Screenshots tell you *when* it broke; this tells you why.
- **A remastered ISO must keep the El Torito UEFI entry's load size.** `xorriso`
  `-boot_image any replay` writes `load size 0` and OVMF refuses it with
  `No mapping`; `keep` preserves the value but can relocate the boot image past
  the 4 GB boundary, which also yields `No mapping`. Building from an extracted
  tree with `-as mkisofs` puts the boot images near the start and works.

## 5a. Windows design (original notes)

### 5.1 Why user mode is not enough

- WFP user-mode filters can permit/block, but they cannot choose a different
  route/interface per app. Windows routes by destination, not process.
- To include only selected apps, the driver must bind/redirect their sockets
  to the tunnel and keep others on the physical interface. That is a kernel
  callout driver (Mullvad's `win-split-tunnel` is the reference).

### 5.2 Options

1. **Upstream Eddie option** (best): an Eddie setting like
   `split.tunnel.apps=include:...` implemented in Eddie's helper. Nimbus
   stays a UI. Requires an AirVPN/Eddie feature request.
2. **Signed WFP driver:** KMDF + WFP callouts, process tracking, interface
   binding. Needs an EV certificate and Microsoft signing; a large,
   security-sensitive project. Reusing Mullvad's GPL-3.0 driver is possible
   in principle but tightly coupled to their daemon.
3. **Proxy workaround** (see 6): only proxy-aware apps; no driver.

### 5.3 Windows built-in per-app VPN

`Windows.Networking.Vpn` supports per-app lists, but the list is based on
package family names (UWP/MSIX), and it needs a VPN plug-in. It is not a
general solution for Win32 apps like Helium with Eddie's WireGuard/OpenVPN
tunnel.

## 6. Cross-platform proxy MVP

A local SOCKS5 proxy in the Nimbus backend:

- listens on 127.0.0.1;
- forces outbound sockets onto the tunnel interface
  (`IP_UNICAST_IF` on Windows, `SO_BINDTODEVICE`/policy on Linux);
- does remote DNS through the tunnel (SOCKS5h);
- Eddie runs in `out` mode so non-proxied apps stay outside;
- launches the target app with `--proxy-server=socks5://127.0.0.1:<port>`
  (Helium/Chromium) or shows the proxy address for manual configuration.

Limits:

- only apps that support a proxy (browsers, many CLI tools);
- the app must be launched with the proxy setting or configured manually;
- not a general per-app solution;
- requires validating Eddie's `out` mode and interface binding on both OSes.

Why it is attractive: no kernel driver, no root daemon, works on both
platforms, and covers the Helium use case.

## 7. UX

Settings → App split tunneling:

- Mode: Include (only selected apps use the VPN) / Exclude.
- App list: search, grouped by app, icons, multi-select.
- Selected list with remove; "applies on next connection" + Reconnect.
- Platform availability: Linux include/exclude; Windows proxy-only until a
  driver or upstream option exists.
- Warnings: included apps' DNS; excluded apps expose the real IP.

## 8. Phases

Decided order for the Linux helper:

1. **Validate `out` mode** — done, §9.1. `scripts/validate-out-mode.mjs` stays
   as the regression test for future Eddie builds.
2. **Helper** — done, `src-tauri/helper` (crate `nimbus-helper`): app
   enumeration, plan generation, cgroup, mark, policy rule, table 100,
   `/proc` watcher, teardown, `status`, and a `--self-test`. Verified end to end
   against a dummy device, §9.2.
3. **Nimbus integration** — done. `serve` is a systemd service
   (`src-tauri/helper/nimbus-helper.service`, installed by
   `scripts/install-helper.sh`); `src-tauri/src/split.rs` is the socket client
   and exposes `split_available` / `split_apps` / `split_status` / `split_begin`
   / `split_end`. Include mode passes `network.ipv4.mode=out` +
   `dns.check=false` + `dns.mode=none`, installs the policy after Eddie reports
   connected, and removes it before disconnecting. A failure to install the
   policy fails **closed**: it disconnects, because in `out` mode nothing is in
   the VPN until the policy lands.
4. **App picker** — done. Settings → App split tunneling lists running apps
   with desktop names, multi-select, search and refresh, persisted under
   `nimbus.appSplit`. The Connect screen states that only those apps use the
   VPN.
5. **DNS** — redirect plus the tunnel-bound forwarder. Without it, included
   apps leak their DNS queries over the physical link.
6. **Harden** — rtnetlink teardown when the tunnel disappears, reconnect
   handling, packaging the helper into the deb/AppImage, polkit policy.
7. **Windows** — upstream Eddie option or signed WFP driver; the proxy MVP
   stays the fallback for proxy-aware apps.

Exclude mode reuses steps 2–3 and skips step 5.

## 9. Validation harness

`scripts/validate-out-mode.mjs` (Linux, Node, no build step):

- snapshots the default route, rules, and resolver before touching anything;
- signs in to a throwaway profile under `/tmp/nimbus-outmode`, so Nimbus's own
  saved session is never modified;
- connects with `--network.ipv4.mode=out --routes.custom=1.1.1.1,in,…` and
  `203.0.113.0/24,in,…` (TEST-NET-3, so only the routing table is observable);
- asserts the default route stayed on the physical interface, both listed
  destinations use the tunnel, an unlisted address does not, and no IPv6
  default appears through the tunnel;
- disconnects and asserts the tunnel device and custom routes are gone and the
  default route is restored;
- writes every engine event to `/tmp/nimbus-outmode/engine.log` and the
  verdicts to `report.json`.

```bash
node scripts/validate-out-mode.mjs --baseline
AIRVPN_USER=… AIRVPN_PASS=… node scripts/validate-out-mode.mjs
node scripts/validate-out-mode.mjs --creds /path/to/creds.env
```

Credentials are passed on eddie-cli's command line because Eddie's jsoninout
`options.set` ignores password-type options, exactly as Nimbus does it. They are
briefly visible in `ps`, so prefer the env var or a `chmod 600` creds file over
`--password` on the command line. The script prints no secrets: only routes,
rules, resolver addresses, and verdicts.

### 9.1 Result (Linux, 2026-11-15)

Both protocols pass 9/9 with `--network.ipv4.mode=out`, `--dns.check=false`,
`--dns.mode=none`, and two `in` routes.

WireGuard (`--protocol=wireguard`):

```
default via 192.168.4.1 dev enp42s0 proto dhcp          ← untouched
1.1.1.1 dev Eddie scope link                            ← custom `in`
203.0.113.0/24 dev Eddie scope link                     ← custom `in`
198.44.129.2 dev Eddie scope link                       ← exit IP
198.54.129.117 via 192.168.4.1 dev enp42s0              ← entry IP
```

OpenVPN (`--protocol=openvpn`):

```
default via 192.168.4.1 dev enp42s0 proto dhcp          ← untouched
1.1.1.1 dev tun0 scope link                             ← custom `in`
10.65.4.0/24 dev tun0 proto kernel scope link           ← tunnel subnet
203.0.113.0/24 dev tun0 scope link                      ← custom `in`
198.44.129.242 dev tun0 scope link                      ← exit IP
198.54.129.123 via 192.168.4.1 dev enp42s0              ← entry IP
```

Verified in both runs:

- the system default route stayed on `enp42s0`;
- both listed destinations resolved to the tunnel device (`Eddie` / `tun0`),
  with the tunnel's own source address;
- `8.8.8.8` stayed on the physical interface with the LAN source address;
- no IPv6 default route appeared (this host has none);
- disconnect removed the tunnel device, both custom routes, and left the
  default route and the resolver (`127.0.0.53`) untouched.

Two findings that change the implementation:

1. **`dns.check=false` is required.** With the default (`true`) the engine
   connects, installs the routes, passes its IPv4/IPv6 route checks, then fails
   "Checking DNS" and loops through "Restart in 3 sec." indefinitely. Any
   include-mode connect must set this.
2. **Do not assume the device name.** WireGuard uses `Eddie` (not `wg0`) and
   OpenVPN uses `tun0`. The helper must read the device from the route or from
   Eddie's session, which the design already requires.

Still unvalidated: a Windows host, and IPv6 (`network.ipv6.mode=out`) on a
machine that actually has IPv6.

### 9.2 Result — helper integration (Linux, 2026-11-15)

`src-tauri/helper` (`nimbus-helper`) is built and tested:

- 13 unit tests covering the app enumeration, `Exec=` parsing, route copying,
  destination validation, and every generated rule;
- `nimbus-helper apps` lists running applications grouped by executable, with
  desktop-entry names and icons (`Discord`, `Dolphin`, …), filtered to the
  invoking user;
- `nimbus-helper plan` prints the exact policy without root.

End-to-end, with a **dummy device standing in for the tunnel** (no VPN session
needed) and an unprivileged probe process that sends three datagrams:

```
probe adopted into cgroup                    PASS
mark rule installed                          PASS
iptables mangle OUTPUT                      3 packets  MARK set 0x4e4d
postrouting, mark + nimbus-test0            3 packets
postrouting, mark + enp42s0                 0 packets
teardown: mark rule / ip rule / table / cgroup   all removed
control run with no policy                  0 packets marked
```

The probe was deliberately run as the desktop user, not as root: the first
attempt failed because the ownership guard refused to adopt a root process,
which is the intended behaviour.

Measured caveat, recorded in §4.3: the same test fails with nftables
`meta mark set` — 3 packets marked, 0 through the tunnel, 3 out of the physical
interface. Only the xtables `MARK` target reroutes.

Bugs the integration caught, both now fixed:

- `cgroup.procs` was written with every PID in one buffer, so selecting a real
  browser failed with "Invalid argument" and nothing was adopted. The first
  dummy-device test only had one process and passed.
- A failure after routes and marks were installed left them behind, because the
  new session object was never created and so never torn down. `begin` now
  undoes everything on any failure.
- The installer used `systemctl enable --now`, which does not restart an
  already-running unit, so upgrades kept executing the previous binary.
- Included apps had no connectivity at all: the mark rerouted their packets
  correctly, but they still carried the source address chosen before the mark
  existed, so the exit dropped them. Fixed with a `MASQUERADE` rule for marked
  traffic; conntrack now expects replies at the tunnel address. This is the kind
  of failure a dummy device cannot show — the source rewrite only matters once a
  real peer validates it.

Known gaps at this stage: DNS is not yet redirected (§4.4), the helper is not
packaged into the deb/AppImage (the install script is manual), and nothing has
been exercised against a real Eddie session yet.

## 10. Open decisions

- Is a root helper acceptable long term? Is a signed Windows driver in scope?
- Which apps beyond Helium should the picker prioritise?
- Should include mode ship before exclude mode, or should exclude mode (no DNS
  work) ship first as the safer rollout?
