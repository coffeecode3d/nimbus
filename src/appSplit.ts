/**
 * App-based split tunneling for Nimbus (Linux).
 *
 * Unlike `splitTunnel.ts`, which excludes destinations through Eddie's own
 * `routes.custom`, this selects *applications*. Eddie has no process awareness,
 * so the work is done by a small root service, `nimbus-helper`, which Nimbus
 * talks to over a Unix socket:
 *
 *  - the selected apps are moved into a cgroup;
 *  - their packets are marked, and a policy rule sends marked packets to a
 *    routing table whose default is the tunnel;
 *  - everything else keeps the physical default route.
 *
 * That is *include* mode: only the chosen apps use the VPN. It requires Eddie to
 * run with `network.ipv4.mode=out`, so the connect path differs from the normal
 * one. See docs/app-split-tunneling-plan.md.
 */

export const APP_SPLIT_KEY = "nimbus.appSplit";

export interface RunningApp {
  /** Executable basename, e.g. `helium-browser`. Stable across restarts. */
  id: string;
  /** Human name from the desktop entry, falling back to the id. */
  name: string;
  /** Icon name from the desktop entry, resolved by the frontend. */
  icon: string | null;
  exePath: string;
  pids: number[];
  uids: number[];
  /** True when a desktop entry describes a GUI application. */
  gui: boolean;
}

export interface AppSplitConfig {
  enabled: boolean;
  /** Executable basenames. Kept as ids so they survive restarts. */
  apps: string[];
}

export const EMPTY_APP_SPLIT: AppSplitConfig = { enabled: false, apps: [] };

export function loadAppSplit(): AppSplitConfig {
  try {
    const raw = localStorage.getItem(APP_SPLIT_KEY);
    if (!raw) return { ...EMPTY_APP_SPLIT };
    const parsed = JSON.parse(raw) as Partial<AppSplitConfig>;
    const apps = Array.isArray(parsed.apps)
      ? parsed.apps.filter((id): id is string => typeof id === "string" && id.length > 0)
      : [];
    return { enabled: parsed.enabled === true, apps };
  } catch {
    return { ...EMPTY_APP_SPLIT };
  }
}

export function saveAppSplit(config: AppSplitConfig): void {
  try {
    localStorage.setItem(APP_SPLIT_KEY, JSON.stringify(config));
  } catch {
    // A full or blocked localStorage must not break connecting.
  }
}

/** True when the config asks for an app session that can actually run. */
export function appSplitActive(config: AppSplitConfig): boolean {
  return config.enabled && config.apps.length > 0;
}

/** Names for the selected ids, using the live list when an app is running. */
export function appSplitLabels(config: AppSplitConfig, running: RunningApp[]): string[] {
  return config.apps.map((id) => running.find((app) => app.id === id)?.name ?? id);
}

/**
 * One-line description for the Connect screen, or null when the feature is not
 * going to change anything about this connection.
 */
export function appSplitSummary(config: AppSplitConfig, running: RunningApp[]): string | null {
  if (!config.enabled) return null;
  const count = config.apps.length;
  if (count === 0) return "No apps selected yet";
  const labels = appSplitLabels(config, running);
  if (labels.length === 1) return `Only ${labels[0]} uses the VPN`;
  if (labels.length === 2) return `Only ${labels[0]} and ${labels[1]} use the VPN`;
  return `Only ${labels.slice(0, 2).join(", ")} and ${labels.length - 2} more use the VPN`;
}
