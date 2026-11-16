/**
 * Platform detection shared by the UI and the engine hooks.
 *
 * App split tunneling is implemented differently per platform:
 *  - Linux: a root service puts the selected apps in a cgroup and steers them
 *    into the tunnel with a firewall mark and a policy route.
 *  - Windows: not available yet. Doing the same thing needs a WFP kernel
 *    driver, and kernel drivers only load if they are signed by Microsoft,
 *    which requires an EV code-signing certificate. Shipping an unsigned
 *    driver would simply be ignored by the kernel on any machine with Secure
 *    Boot enabled.
 *
 * Using one constant keeps the UI and the connect path in agreement: the
 * frontend must never ask Eddie for `network.ipv4.mode=out` on a platform where
 * the helper cannot put anything back into the tunnel.
 */
export const IS_WINDOWS =
  typeof navigator !== "undefined" && /Windows/i.test(navigator.userAgent);

/** True when app split tunneling can be used on this platform at all. */
export const APP_SPLIT_PLATFORM_SUPPORTED = !IS_WINDOWS;
