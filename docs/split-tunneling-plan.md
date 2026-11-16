# Split tunneling in Nimbus — research and implementation plan

Status: Phase 1 (IPv4/subnet exclude mode) implemented. Phases 2–3 remain
proposals.

## TL;DR

- Every major VPN does **app-based exclusion** on Windows with a kernel driver
  (WFP), and on Linux either with **cgroups + firewall marks** (Mullvad,
  Proton) or with **IP/subnet allowlists** (Nord).
- Nimbus cannot do app-based exclusion without adding a new privileged
  component. That would break the "UI only, Eddie owns root" boundary and
  would need a signed Windows driver.
- Nimbus **can** do IP/subnet exclusion today, cross-platform, with **zero
  changes to Eddie, its helper, or its integrity checks**, because Eddie
  already exposes `routes.custom`.
- Recommendation: ship **IP/subnet exclude mode** first (small, safe,
  genuinely useful), then decide separately whether app-based is worth a
  privileged helper.

## 1. How other clients do it

| Client | Platform | Mechanism | Granularity | Notes |
|---|---|---|---|---|
| Mullvad | Linux | cgroup `net_cls` + nftables marks + policy routing | Per process / PID | `mullvad-exclude <cmd>`, `mullvad split-tunnel add <pid>`; children inherit; still on cgroup v1 while v2 migration is finished |
| Mullvad | Windows | Signed KMDF driver `win-split-tunnel` + WFP callouts | Per executable path | Tracks process tree, redirects socket binds, blocks in-tunnel traffic, handles IPv6; needs a kernel driver |
| Proton | Linux | cgroup + system D-Bus service | Per app, optionally IP ranges | Exclude and include modes; affected apps must be restarted after connecting; official distro packages only |
| Proton | Windows | WFP-based driver | Per app and IP | Both kill-switch modes compatible |
| Nord | Linux | Allowlist of ports/subnets | Ports / subnets, not apps | Matching traffic bypasses the tunnel **and** the kill switch; inbound and outbound |
| OpenVPN / WireGuard | Any | `route` directives / `AllowedIPs` | Destinations only | No process awareness; the OS decides which app uses which route |

Mullvad's design note is the important one: their model sorts apps into
**Excluded / Included / Undecided**, and new apps start *undecided* so they
cannot leak while classification is in flight. That level of correctness is
only possible with kernel-level process tracking.

Common UX across clients:

- Exclude is the default; include is an advanced/inverse mode.
- A clear warning that excluded traffic exposes the real IP.
- "Reconnect for changes to take effect."
- On Linux, apps already running when the tunnel comes up usually need a
  restart before the rule applies.

## 2. What Eddie already provides

Eddie 2.26.2 documents this option (verified with
`eddie-cli --help`):

```
routes.custom
  Custom routes. Format: '{ip or range},{in/out},{note (optional)};...'.
  Separate multiple routes with ;.
  Example: '1.2.3.4,in;2.3.4.5/23,out,test'.
```

The implementation is in
`src/Lib.Core/ConnectionTypes/IConnectionType.cs` (`SetupRoutes`) and
`src/Lib.Core/Session.cs` (`RoutesApply`):

- `out` = send this destination through the **physical** gateway/interface.
- `in` = send this destination through the **tunnel** gateway/interface.
- `out` only takes effect when the address family is fully routed (the
  normal "route everything through the VPN" mode), which is Nimbus's
  default.
- `out` routes are installed in the `pre` phase (before the tunnel comes up)
  and removed in the `post` phase (on disconnect).
- `in` routes are installed in the `up` phase and removed in `post`.
- Eddie's network-lock plugin already adds every `routes.custom` `out`
  destination to its allowlist, so the two features are designed to coexist.
- Route application is cross-platform: `ip route` on Linux, the elevated
  `route` command on Windows.
- The option is a normal CLI/profile option, so Nimbus can pass
  `--routes.custom=...` when spawning the engine. An empty value overrides a
  stale value saved in the profile.

This is, in effect, Eddie's built-in **destination-based split tunneling**.
The official Eddie GUI already exposes it as "Custom routes"; Nimbus can make
it understandable.

### What Eddie does *not* provide

- No cgroup, PID, process-name, or executable-path matching.
- No WFP integration.
- No app picker, no child-process inheritance, no per-app leak protection.
- No automatic domain resolution for routes.

## 3. Feasibility verdict

| Capability | Feasible in Nimbus today? | How |
|---|---|---|
| Exclude IPs / subnets from the tunnel | **Yes** | `routes.custom` with `out`; no helper changes |
| Include only selected IPs/subnets | **Yes, but risky** | `network.ipv4.mode=out` + `routes.custom` `in`; changes the whole tunnel model |
| Exclude by domain name | **Partly** | Resolve at connect, pin the resulting IPs; DNS and CDN caveats |
| Exclude apps on Linux | **No, not without a new root helper** | cgroup + nftables, or a namespace launcher |
| Exclude apps on Windows | **No** | Requires a signed WFP kernel driver |
| Exclude apps on macOS | **No** | Requires a Network Extension |

The important architectural point: app-based split tunneling is not a UI
feature. It is a privileged networking feature. Nimbus deliberately does not
talk to root, and Eddie's helper only accepts a fixed command set. Adding
app-based exclusion means either:

1. contributing a split-tunnel option upstream to Eddie, or
2. shipping a second privileged helper from Nimbus.

Both are larger projects than the UI work and both widen the attack surface.
They should be a separate decision.

## 4. Phase 1 — IP/subnet exclude mode

### 4.1 User experience

Add a **Split tunneling** section in **Settings**, between Connection and
Network protection:

```
Split tunneling                                      [ off ]
Send chosen IPv4 addresses and subnets through your normal connection
instead of the VPN.

[ Local network ]  [ Multicast ]

  192.168.0.0/16     Home LAN                 [x]
  203.0.113.7        Office printer           [x]

  [ 192.168.0.0/16 or 203.0.113.7 ] [ Label (optional) ] [ Add ]

  Anything you exclude is not protected by the VPN. Those sites see your
  real IP address and your ISP can see the traffic.

  Applies on your next connection.                       [ Reconnect ]
```

Details:

- Toggle is off by default.
- Presets: **Local network** adds `10.0.0.0/8`, `172.16.0.0/12`,
  `192.168.0.0/16`, `169.254.0.0/16`; **Multicast** adds `224.0.0.0/4`.
  Presets are normal removable rules, not a hidden mode.
- "Add destination" accepts an IPv4 address or CIDR. IPv6 is rejected with a
  clear message in Phase 1 (see 4.5).
- Optional label, shown in the list and passed to Eddie as the route note.
- Duplicate and overlapping rules are rejected with a clear message.
- If the tunnel is connected and the list differs from the list used for the
  current session, show **Applies on your next connection** and a
  **Reconnect** button. The Connect screen also replaces "all traffic is
  tunneled" with the number of excluded destinations.

### 4.2 Data model

```ts
interface SplitTunnelRule {
  id: string;
  cidr: string;        // normalized, e.g. "192.168.0.0/16"
  label: string;       // sanitized route note, may be empty
  preset?: "lan" | "multicast";
}

interface SplitTunnelConfig {
  enabled: boolean;
  rules: SplitTunnelRule[];
}
```

Phase 1 is exclude-only, so there is no `mode` field. The implementation lives
in `src/splitTunnel.ts` (validation, presets, serialization, persistence),
`src/liveEngine.ts` / `src/engine.ts` (state and connect wiring),
`src/screens/Settings.tsx` (UI), and `src-tauri/src/engine.rs`
(`--routes.custom`).

Persist under `nimbus.splitTunnel` in `localStorage`, next to the other
Nimbus preferences. The engine never stores the authoritative copy; it only
receives the serialized string for the connecting session.

Serialization:

```
192.168.0.0/16,out,Home LAN;203.0.113.7,out,Office printer
```

Rules:

- Strip `;`, `,`, newlines, and control characters from labels.
- Reject `0.0.0.0/0` in exclude mode: excluding everything would silently
  disable the VPN. IPv6 is rejected wholesale in Phase 1.
- Normalize host bits (`192.168.1.7/24` → `192.168.0.0/24`) and tell the
  user.
- Cap the list at 64 rules so the argv stays sane.
- Validate in TypeScript for immediate feedback. Rust passes the value
  through untouched: Eddie's own parser skips malformed entries, and the
  string is always one argv element, so there is nothing to escape.

### 4.3 Engine wiring

Rust (`src-tauri/src/engine.rs`):

- Add `routes_custom: Option<String>` to `StartOptions`.
- In `build_args`, push `--routes.custom=<value>` when the field is present.
  `Some("")` clears a value left in the profile; `None` leaves it alone.
- Nimbus sends `Some(...)` on every spawn (idle and connecting), so a
  disabled feature overrides any stale profile value on the next session.
- Add a unit test asserting the flag is present, that an empty value is still
  sent, and that an omitted value leaves the profile alone.

Frontend:

- `src/splitTunnel.ts`: types, parser/validator, presets, serializer,
  localStorage read/write.
- `src/types.ts`: add the split-tunnel fields and `reconnect` to `EngineApi`.
- `src/liveEngine.ts`: state, setter, persistence, the `splitTunnelApplied`
  string used for the current session, and the reconnect action.
- `src/engine.ts` (mock): same surface, so previews and screenshots work.
- `src/screens/Settings.tsx`: the UI described above.
- `src/screens/Connect.tsx`: the connected subtitle names the number of
  excluded destinations instead of claiming all traffic is tunneled.

The `routes.custom` string is built from the **current** config at connect
time, not at app start, so a change made while disconnected is picked up by
the next connect without a restart.

### 4.4 Validation and failure behavior

- Eddie skips an entry it cannot parse and logs "Custom route invalid"; it
  does not abort the connection. Nimbus should still reject bad input
  before it reaches Eddie.
- A valid route that the OS cannot install can abort the connect (Eddie
  throws on a failed `add`). The realistic case is an IPv6 rule on a host
  with no IPv6. Phase 1 rejects IPv6 input entirely so this cannot happen.
- If a route conflicts with an existing route, Eddie logs it and continues.
- If the engine crashes, routes are cleaned up by Eddie's recovery path on
  the next start; Nimbus should not try to edit the routing table itself.

### 4.5 Platform and protocol behavior

| Case | Behavior |
|---|---|
| Linux + WireGuard | `out` route to the physical gateway; the tunnel's `/1` routes are less specific, so the exclusion wins |
| Linux + OpenVPN | Same; Eddie manages the routes and ignores the pushed `redirect-gateway` |
| Windows + WireGuard | Same, via the elevated `route` command |
| Windows + OpenVPN | Eddie manages the routes too (OpenVPN's `redirect-gateway` is ignored); the more specific `out` route wins |
| IPv4 | Works in all four combinations |
| IPv6 | Default Eddie mode is `in-block`; if the server or host has no IPv6, Eddie skips IPv6 routes. Phase 1 rejects IPv6 input and documents IPv4-only |
| DNS | Unchanged. Excluded apps still use Eddie's DNS, so a hostname can resolve to an address that is not excluded. Document this; prefer CIDRs |
| Network lock | Nimbus keeps it off for now. If it is re-enabled, Eddie already allowlists `out` routes, but the UI must say that excluded destinations bypass the lock too |
| Disconnect | Eddie removes the `out` routes in the `post` phase; normal routing returns |

### 4.6 Security and privacy copy

The UI must be blunt, in the same spirit as Mullvad:

> Excluded traffic does not go through the VPN. Those destinations see your
> real IP address, and your ISP can see the traffic. If a hostname resolves
> to several addresses, only the excluded ones bypass the tunnel.

Also warn about the de-anonymization case: excluding `example.com` but not
`assets.example.com` can let the same observer see the same user from two
addresses at once.

## 5. Phase 2 — optional extensions

These are deliberately separate from Phase 1.

### 5.1 Domain pinning

Let the user add a hostname; resolve A/AAAA at connect time and pin the
resulting addresses into `routes.custom`. Re-resolve on reconnect. Caveats:
CDNs rotate addresses, DNS still goes through the tunnel, and the pinned set
can go stale. Useful for "exclude this service", not for privacy-critical
traffic.

### 5.2 Include mode (IP/subnet)

Set `network.ipv4.mode=out` (and the IPv6 equivalent) and add `in` routes.
Only listed destinations use the VPN; everything else uses the physical
connection. This is a much bigger behavioral change and a bigger footgun
than exclude mode. It also needs its own DNS story. Recommend keeping it
behind an "Advanced" disclosure, or not shipping it.

### 5.3 App-based exclusion on Linux

The only correct implementation is the Mullvad/Proton approach:

- a cgroup for excluded processes (v2 `net_cls`/BPF, or v1 `net_cls`),
- nftables/iptables marks and a policy route that sends marked packets out
  the physical interface,
- rules that block excluded apps from using the tunnel and block unmarked
  apps from leaving it,
- process-start tracking and child inheritance,
- coordination with Eddie's network lock and route lifecycle.

This needs a root daemon. It is roughly "a second network lock" and should
not be bolted onto the UI. If it is ever built, Linux first, with a clear
statement that Windows parity is not planned.

### 5.4 Upstream alternative

The cleanest long-term path is an Eddie option such as
`split.tunnel.exclude.apps` implemented inside Eddie's privileged helper.
Nimbus would then stay a UI. This is an AirVPN/Eddie feature request, not a
Nimbus patch.

## 6. Effort estimate

| Work | Estimate |
|---|---|
| Phase 1 UI + validation + persistence | 1–2 days |
| Phase 1 Rust flag + tests | half a day |
| Phase 1 Linux validation (WireGuard + OpenVPN) | half a day |
| Phase 1 Windows validation | blocked on a Windows host |
| Phase 2 domain pinning | 1–2 days |
| Phase 2 include mode | 2–3 days, plus a lot of testing |
| Phase 3 Linux app-based spike | 1–2 weeks, new privileged component |
| Phase 3 Windows app-based | out of scope (signed kernel driver) |

## 7. Test plan

Automated (already in place):

- Pure-function checks for the CIDR parser/normalizer: valid IPv4, host
  bits, `/0` rejection, out-of-range octets/prefixes, leading zeros, IPv6
  rejection, overlap detection, label sanitization, serialization, and the
  disabled → empty override.
- Rust `build_args`: flag present, empty value still sent, omitted value
  absent, semicolons/commas preserved.

Not yet automated: there is no TypeScript test runner in the project, so the
parser checks are run ad hoc with `node --experimental-strip-types`.

Manual, Linux (pending):

1. Add `192.0.2.0/24` as an excluded destination, connect.
2. `ip route get 192.0.2.1` shows the physical interface; a non-excluded
   address shows the tunnel interface.
3. `curl` to an excluded test endpoint reports the real IP; a non-excluded
   endpoint reports the VPN exit IP.
4. Disconnect; confirm the `out` route is gone.
5. Disable split tunneling, reconnect; confirm no stale `out` route.
6. Repeat with OpenVPN.
7. Change the list while connected; confirm the UI says "applies on your next
   connection" and the old route is still in effect until reconnect.
8. Disable split tunneling while connected; confirm the Reconnect prompt
   still appears.

Manual, Windows (pending a Windows host):

1. `route print` shows the exclusion on the physical interface index.
2. `Find-NetRoute -RemoteIPAddress <excluded>` picks the physical interface.
3. Disconnect removes it.
4. Repeat with OpenVPN.

Regression:

- Normal connect/disconnect, server switch, latency probe, update flow.
- No credential appears in logs or argv beyond the existing handoff.
- A disabled feature leaves no `routes.custom` override on the next session.

## 8. Decisions

Recorded for Phase 1:

1. **Scope:** IP/subnet only. App-based exclusion remains a separate project.
2. **Mode:** exclude only. Include mode is not shipped.
3. **Platform:** Linux is the first validation target; its runtime checks are
   still pending, as is Windows runtime validation.
4. **Presets:** "Local network" and "Multicast" only. Domain-based service
   presets need Phase 2 address pinning.
