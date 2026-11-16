/**
 * Platform detection shared by the UI and the engine hooks.
 *
 * App split tunneling means different things per platform:
 *  - Linux: *include* mode. A root service puts the selected apps in a cgroup
 *    and steers them into the tunnel with a firewall mark and a policy route.
 *    Eddie runs with `network.ipv4.mode=out`, so only those apps use the VPN.
 *  - Windows: *exclude* mode. Per-application routing needs a kernel driver, and
 *    Windows only loads drivers signed by Microsoft. Nimbus drives Mullvad's
 *    signed GPL-3.0 driver instead, which can only push apps *out* of the
 *    tunnel. Eddie stays in its normal `in` mode and the selected apps bypass
 *    the VPN.
 *
 * The two are opposite in wording and in what happens to everything else, so the
 * UI has to say which one it is doing rather than reuse one sentence.
 */

export const IS_WINDOWS =
  typeof navigator !== "undefined" && /Windows/i.test(navigator.userAgent);

/** Which way round the selection works. */
export const APP_SPLIT_MODE: "include" | "exclude" = IS_WINDOWS ? "exclude" : "include";
