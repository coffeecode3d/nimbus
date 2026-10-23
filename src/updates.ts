import { useCallback, useEffect, useRef, useState } from "react";
import { BundleType, getBundleType, getVersion } from "@tauri-apps/api/app";
import { check } from "@tauri-apps/plugin-updater";
import type { Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

const CHECK_INTERVAL = 6 * 60 * 60 * 1000;
const LAST_CHECK_KEY = "nimbus.updates.last-check";
const AUTO_CHECK_KEY = "nimbus.updates.auto-check";

export interface UpdateInfo {
  version: string;
  date?: string;
  body?: string;
}

export interface UpdatesApi {
  /** True inside the desktop shell. Browser previews cannot update themselves. */
  enabled: boolean;
  currentVersion: string;
  bundleKind: BundleType | "development";
  /** False for .deb/.rpm development builds, where the package manager owns updates. */
  supported: boolean;
  autoCheck: boolean;
  setAutoCheck: (value: boolean) => void;
  checking: boolean;
  installing: boolean;
  /** Download progress, 0–100, or null when the server did not send a size. */
  progress: number | null;
  info: UpdateInfo | null;
  upToDate: boolean;
  error: string | null;
  lastChecked: number | null;
  check: (manual?: boolean) => Promise<void>;
  install: () => Promise<void>;
  clearError: () => void;
}

function supportedBundle(kind: BundleType | "development"): boolean {
  return (
    kind === BundleType.AppImage ||
    kind === BundleType.Nsis ||
    kind === BundleType.Msi ||
    kind === BundleType.App
  );
}

function readAutoCheck(): boolean {
  try {
    return localStorage.getItem(AUTO_CHECK_KEY) !== "0";
  } catch {
    return true;
  }
}

function readLastCheck(): number | null {
  try {
    const value = Number(localStorage.getItem(LAST_CHECK_KEY));
    return Number.isFinite(value) && value > 0 ? value : null;
  } catch {
    return null;
  }
}

function reason(err: unknown): string {
  const text = err instanceof Error ? err.message : String(err);
  return text.replace(/^Error:\s*/i, "").trim() || "The update check failed.";
}

function friendlyError(err: unknown): string {
  const text = reason(err);
  if (/404|not found|release json|valid release|failed to fetch/i.test(text)) {
    return "No public update feed is reachable yet. Nimbus publishes updates on GitHub Releases; once the repository is public this check will work automatically.";
  }
  return text;
}

/**
 * Signed, in-app updates for Nimbus itself (not the Eddie engine).
 *
 * Checks run at most every six hours when the app starts, and the user can
 * always ask for one from Settings. Installation is deliberately explicit:
 * a VPN client should not restart itself while a tunnel is up.
 */
export function useUpdates(enabled: boolean): UpdatesApi {
  const [currentVersion, setCurrentVersion] = useState("0.1.3");
  const [bundleKind, setBundleKind] = useState<BundleType | "development">("development");
  const [autoCheck, setAutoCheckState] = useState(readAutoCheck);
  const [checking, setChecking] = useState(false);
  const [installing, setInstalling] = useState(false);
  const [progress, setProgress] = useState<number | null>(null);
  const [info, setInfo] = useState<UpdateInfo | null>(null);
  const [upToDate, setUpToDate] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [lastChecked, setLastChecked] = useState<number | null>(readLastCheck);

  const updateRef = useRef<Update | null>(null);
  const checkInFlight = useRef(false);
  const mounted = useRef(true);

  const supported = enabled && supportedBundle(bundleKind);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      const update = updateRef.current;
      updateRef.current = null;
      if (update) void update.close().catch(() => undefined);
    };
  }, []);

  useEffect(() => {
    if (!enabled) return;
    void getVersion()
      .then((version) => {
        if (mounted.current) setCurrentVersion(version);
      })
      .catch(() => undefined);
    void getBundleType()
      .then((kind) => {
        if (mounted.current) setBundleKind(kind);
      })
      .catch(() => {
        if (mounted.current) setBundleKind("development");
      });
  }, [enabled]);

  const setAutoCheck = useCallback((value: boolean) => {
    setAutoCheckState(value);
    try {
      localStorage.setItem(AUTO_CHECK_KEY, value ? "1" : "0");
    } catch {
      // Private browsing or a locked store only loses the preference.
    }
  }, []);

  const clearError = useCallback(() => setError(null), []);

  const checkUpdate = useCallback(
    async (manual = false) => {
      if (!enabled) {
        if (manual) setError("Update checks are only available in the desktop app.");
        return;
      }
      if (checkInFlight.current) return;
      checkInFlight.current = true;
      if (manual) {
        setChecking(true);
        setError(null);
        setUpToDate(false);
      }

      try {
        const next = await check({ timeout: 30_000 });
        if (!mounted.current) {
          if (next) void next.close().catch(() => undefined);
          return;
        }

        const previous = updateRef.current;
        updateRef.current = next;
        if (previous && previous !== next) void previous.close().catch(() => undefined);

        if (next) {
          setInfo({ version: next.version, date: next.date, body: next.body });
          setUpToDate(false);
        } else {
          setInfo(null);
          setUpToDate(manual);
        }
      } catch (err) {
        // Automatic checks stay quiet; otherwise a private repository or an
        // offline laptop would produce a toast every launch.
        if (manual && mounted.current) setError(friendlyError(err));
      } finally {
        const now = Date.now();
        try {
          localStorage.setItem(LAST_CHECK_KEY, String(now));
        } catch {
          // Preference persistence is best-effort.
        }
        if (mounted.current) {
          setLastChecked(now);
          if (manual) setChecking(false);
        }
        checkInFlight.current = false;
      }
    },
    [enabled],
  );

  useEffect(() => {
    if (!enabled || !autoCheck) return;
    const last = readLastCheck() ?? 0;
    const first = window.setTimeout(() => {
      if (Date.now() - last >= CHECK_INTERVAL) void checkUpdate(false);
    }, 4_000);
    const timer = window.setInterval(() => void checkUpdate(false), CHECK_INTERVAL);
    return () => {
      window.clearTimeout(first);
      window.clearInterval(timer);
    };
  }, [enabled, autoCheck, checkUpdate]);

  const install = useCallback(async () => {
    const update = updateRef.current;
    if (!update) {
      setError("Check for updates first.");
      return;
    }
    if (!supported) {
      setError(
        "This build cannot replace itself. Use the AppImage or the Windows installer for in-app updates, or update the .deb with your package manager.",
      );
      return;
    }

    setInstalling(true);
    setProgress(0);
    setError(null);
    let downloaded = 0;
    let total = 0;

    try {
      await update.downloadAndInstall(
        (event) => {
          if (event.event === "Started") {
            total = event.data.contentLength ?? 0;
            downloaded = 0;
            setProgress(total > 0 ? 0 : null);
          } else if (event.event === "Progress") {
            downloaded += event.data.chunkLength;
            if (total > 0) {
              setProgress(Math.min(99, Math.round((downloaded / total) * 100)));
            }
          } else if (event.event === "Finished") {
            setProgress(100);
          }
        },
        { timeout: 10 * 60_000 },
      );

      setProgress(100);
      // On Windows the installer takes over and restarts Nimbus itself. On
      // AppImage and macOS the old process must relaunch into the new bundle.
      if (bundleKind !== BundleType.Nsis && bundleKind !== BundleType.Msi) {
        await relaunch();
      }
    } catch (err) {
      if (mounted.current) {
        setError(`The update could not be installed: ${reason(err)}`);
        setInstalling(false);
        setProgress(null);
      }
    }
  }, [bundleKind, supported]);

  return {
    enabled,
    currentVersion,
    bundleKind,
    supported,
    autoCheck,
    setAutoCheck,
    checking,
    installing,
    progress,
    info,
    upToDate,
    error,
    lastChecked,
    check: checkUpdate,
    install,
    clearError,
  };
}
