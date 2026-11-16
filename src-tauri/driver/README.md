Mullvad split tunnel driver
===========================

`mullvad-split-tunnel.sys` is the signed kernel driver from the Mullvad VPN
app. Nimbus ships it unmodified and drives it over its documented IOCTL
interface; it is not a fork and contains no Nimbus code.

- Source:    MullvadVPN-2026.5.exe from https://cdn.mullvad.net/app/desktop/releases/2026.5/
             (extracted from `C:\Program Files\Mullvad VPN\resources\`)
- Size:      98400 bytes
- MD5:       c22ca4be33e425bc46af5dcdf21230c5
- Copyright: Mullvad VPN AB
- Licence:   GPL-3.0, https://github.com/mullvad/win-split-tunnel

Why it is here
--------------

Per-application routing on Windows needs a WFP callout driver, and Windows only
loads drivers signed by Microsoft, which means an EV code-signing certificate
issued to a verified business. Mullvad publishes theirs under GPL-3.0, already
signed, so reusing it is the only free path to this feature. See `NOTICE` and
`docs/app-split-tunneling-plan.md`.

The driver implements *exclude* mode only: listed images are pushed out of the
tunnel, everything else stays in. Nimbus therefore presents Windows as "apps
that bypass the VPN", not "apps that use the VPN".

Refreshing
----------

Download a newer Mullvad installer, extract this one file from it, and update
the size and MD5 above. Verify that `driver` still speaks the same IOCTL
protocol before shipping: `helper-win/src/proto.rs` mirrors `src/defs/*.h` from
upstream, and the two must stay in step.

Notes for the service
---------------------

- The driver never creates the WFP sublayers its filters attach to; the client
  must register them first. `helper-win/src/firewall.rs` does that.
- Its control device is exclusive, so exactly one process may hold it.
- It keeps state across clients, so a new client has to reset it.
