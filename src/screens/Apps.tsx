import { useEffect, useState } from "react";
import type { Engine } from "../engine";
import { Button, Field, Section, Toggle } from "../components";
import { Apps as AppsIcon, X } from "../icons";
import { APP_SPLIT_PLATFORM_SUPPORTED } from "../platform";

/**
 * App split tunneling, as a first-class page rather than a Settings subsection.
 *
 * Include mode only: the selected applications use the VPN and everything else
 * keeps the normal connection. The work is done by the `nimbus-helper` system
 * service; Nimbus only sends it a list of executable names.
 */
export function AppsScreen({
  engine,
  notify,
}: {
  engine: Engine;
  notify: (msg: string) => void;
}) {
  const [query, setQuery] = useState("");
  const split = engine.appSplit;

  // Refresh on mount so the picker reflects what is running now. Keyed on the
  // callback, not on `engine`: the hook returns a fresh object every render.
  const refreshApps = engine.refreshApps;
  useEffect(() => {
    void refreshApps();
  }, [refreshApps]);

  const queryText = query.trim().toLowerCase();
  const visible = engine.runningApps.filter(
    (app) =>
      !queryText ||
      app.name.toLowerCase().includes(queryText) ||
      app.id.toLowerCase().includes(queryText),
  );

  const toggleApp = (id: string) => {
    const has = split.apps.includes(id);
    engine.setAppSplit({
      enabled: true,
      apps: has ? split.apps.filter((app) => app !== id) : [...split.apps, id],
    });
  };

  const activeCount = engine.connected ? (engine.appSplitApplied?.length ?? 0) : 0;
  const pending =
    engine.appSplitApplied !== null &&
    engine.appSplitApplied.join(",") !== split.apps.join(",") &&
    (engine.connected || engine.state === "connecting");

  return (
    <div className="settings">
      <header className="page-head">
        <div>
          <h1>Apps</h1>
          <p>Choose which applications use the VPN. Everything else keeps your normal connection.</p>
        </div>
        <div className="apps-status">
          <span className={`apps-dot${activeCount > 0 ? " is-live" : ""}`} />
          {!APP_SPLIT_PLATFORM_SUPPORTED
            ? "Not available on Windows"
            : activeCount > 0
              ? `${activeCount} app${activeCount === 1 ? "" : "s"} in the tunnel`
              : engine.connected
                ? "Tunnel up, no app policy"
                : "Not connected"}
        </div>
      </header>

      <div className="settings-scroll">
        <Section
          title="App split tunneling"
          note="Linux only for now. A small root service installs a cgroup, a firewall mark and a policy route; Nimbus itself never runs as root, and it can only ever steer your own processes."
        >
          {APP_SPLIT_PLATFORM_SUPPORTED ? (
            <Field
              title="Only selected apps use the VPN"
              desc="Pick one or several running apps. Everything else stays outside the tunnel."
            >
              <Toggle
                label="App split tunneling"
                checked={split.enabled}
                onChange={(enabled) => engine.setAppSplit({ ...split, enabled })}
              />
            </Field>
          ) : (
            <p className="split-warning" role="status">
              <strong>Not available on Windows yet.</strong> Steering individual applications on
              Windows needs a kernel-mode network filter driver, and Windows only loads drivers
              signed by Microsoft, which requires an EV code-signing certificate. On Windows,
              Nimbus protects everything or nothing. An exclude-mode option is planned.
            </p>
          )}

          {APP_SPLIT_PLATFORM_SUPPORTED && !engine.appSplitAvailable && (
            <p className="split-warning" role="status">
              The helper service is not installed. Install Nimbus from the .deb package (which
              sets it up), or run <code>sudo scripts/install-helper.sh</code> once from the source
              checkout.
            </p>
          )}

          {engine.appSplitAvailable && split.enabled && (
            <div className="split-panel">
              <div className="app-toolbar">
                <input
                  className="text-input split-input"
                  type="text"
                  value={query}
                  placeholder="Search running apps"
                  aria-label="Search running apps"
                  spellCheck={false}
                  onChange={(e) => setQuery(e.target.value)}
                />
                <Button variant="quiet" onClick={() => void refreshApps()}>
                  Refresh
                </Button>
              </div>

              {split.apps.length > 0 ? (
                <ul className="split-rules">
                  {split.apps.map((id) => {
                    const app = engine.runningApps.find((entry) => entry.id === id);
                    return (
                      <li key={id} className="split-rule">
                        <AppsIcon size={14} />
                        <span className="split-cidr">{app?.name ?? id}</span>
                        <span className="split-rule-label mono">{id}</span>
                        <button
                          type="button"
                          className="split-remove"
                          aria-label={`Remove ${app?.name ?? id}`}
                          title="Remove"
                          onClick={() => toggleApp(id)}
                        >
                          <X size={13} />
                        </button>
                      </li>
                    );
                  })}
                </ul>
              ) : (
                <p className="split-empty">
                  No apps selected yet. Pick one below — Helium, for example.
                </p>
              )}

              {visible.length > 0 ? (
                <ul className="app-list">
                  {visible.map((app) => {
                    const selected = split.apps.includes(app.id);
                    return (
                      <li key={app.id}>
                        <label className={`app-row${selected ? " selected" : ""}`}>
                          <input
                            type="checkbox"
                            checked={selected}
                            onChange={() => toggleApp(app.id)}
                          />
                          <span className="app-name">{app.name}</span>
                          <span className="app-id mono">{app.id}</span>
                          <span className="app-pids">{app.pids.length} pid</span>
                        </label>
                      </li>
                    );
                  })}
                </ul>
              ) : (
                <p className="split-empty">
                  {engine.runningApps.length === 0
                    ? "No running applications were reported by the helper."
                    : "No running app matches that search."}
                </p>
              )}

              {engine.appSplitError && (
                <p className="split-error" role="alert">
                  {engine.appSplitError}
                </p>
              )}

              <p className="split-warning">
                Selected apps send their traffic — and their DNS queries — through the VPN;
                everything else keeps your real IP address. Restart an app after selecting it so its
                connections are re-created. Because DNS is forced through the tunnel, local-only
                names (`.local`, your router's search domain) will not resolve for these apps.
              </p>
            </div>
          )}

          {pending && (
            <div className="split-pending">
              <span>Applies on your next connection.</span>
              <Button variant="quiet" onClick={engine.reconnect}>
                Reconnect
              </Button>
            </div>
          )}
        </Section>

        <Section
          title="How it works"
          note={
            APP_SPLIT_PLATFORM_SUPPORTED
              ? "Worth knowing before you rely on it."
              : "Why Windows is different."
          }
        >
          {!APP_SPLIT_PLATFORM_SUPPORTED && (
            <ul className="apps-notes">
              <li>
                Windows routes by destination, not by process. Choosing apps needs a driver that
                binds their sockets to the tunnel adapter, and that driver must be signed by
                Microsoft before Windows will load it.
              </li>
              <li>
                Signing requires an EV code-signing certificate issued to a verified business, so
                it is a procurement step rather than a code change.
              </li>
              <li>
                Until then, everything on Windows goes through the VPN or nothing does. The
                destination-based exclusions in Settings → Split tunneling work on both platforms.
              </li>
            </ul>
          )}

          {APP_SPLIT_PLATFORM_SUPPORTED && (
            <>
              <ul className="apps-notes">
                <li>
                  Only <strong>running</strong> apps are listed. Open Helium first, then pick it.
                </li>
                <li>
                  Connections that existed before the policy was installed stay where they were,
                  so restart or reload the app after enabling this.
                </li>
                <li>
                  The tunnel carries only the selected apps. If the policy cannot be installed,
                  Nimbus disconnects instead of leaving everything outside the VPN.
                </li>
                <li>
                  Destination exclusions (Settings → Split tunneling) and app selection are
                  mutually exclusive: choose one or the other.
                </li>
              </ul>
              <div className="apps-links">
                <Button variant="quiet" onClick={() => void refreshApps()}>
                  Re-scan running apps
                </Button>
                <Button
                  variant="quiet"
                  onClick={() => {
                    engine.setAppSplit({ enabled: false, apps: split.apps });
                    notify("App split tunneling turned off for the next connection");
                  }}
                >
                  Turn off
                </Button>
              </div>
            </>
          )}
        </Section>
      </div>
    </div>
  );
}
