import { useState } from "react";
import type { Engine } from "../engine";
import type { UpdatesApi } from "../updates";
import { ACCENTS } from "../appearance";
import type { Appearance } from "../appearance";
import { Field, Section, Select, Toggle, Button, Spinner } from "../components";
import { Check, X } from "../icons";
import {
  MAX_SPLIT_RULES,
  SPLIT_TUNNEL_PRESETS,
  findOverlap,
  newRuleId,
  parseCidr,
  presetLabel,
  presetRules,
  sanitizeLabel,
  serializeSplitTunnel,
} from "../splitTunnel";
import type { SplitTunnelPreset, SplitTunnelRule } from "../splitTunnel";

export function SettingsScreen({
  engine,
  appearance,
  updates,
  notify,
  onOpenApps,
}: {
  engine: Engine;
  appearance: Appearance;
  updates: UpdatesApi;
  notify: (msg: string) => void;
  onOpenApps: () => void;
}) {
  const installUpdate = async () => {
    if (engine.connected || engine.state === "connecting" || engine.state === "disconnecting") {
      notify("Disconnect before installing an update");
      return;
    }
    await engine.stopForUpdate();
    await updates.install();
  };
  const checkedLabel = updates.lastChecked
    ? new Date(updates.lastChecked).toLocaleString([], {
        month: "short",
        day: "numeric",
        hour: "2-digit",
        minute: "2-digit",
      })
    : "not yet";
  const [dnsBlock, setDnsBlock] = useState(true);
  const [blockV6, setBlockV6] = useState(true);
  const [autoConnect, setAutoConnect] = useState(false);
  const [launchAtLogin, setLaunchAtLogin] = useState(false);
  const [notifications, setNotifications] = useState(true);
  const [autoReconnect, setAutoReconnect] = useState(true);
  const [mode, setMode] = useState("direct");
  const [splitInput, setSplitInput] = useState("");
  const [splitLabel, setSplitLabel] = useState("");
  const [splitError, setSplitError] = useState<string | null>(null);

  const split = engine.splitTunnel;
  const splitRoutes = serializeSplitTunnel(split);
  const splitPending =
    engine.splitTunnelApplied !== null &&
    engine.splitTunnelApplied !== splitRoutes &&
    (engine.connected || engine.state === "connecting");

  const addSplitRule = (raw: string, label: string) => {
    const parsed = parseCidr(raw);
    if (!parsed.ok) {
      setSplitError(parsed.error);
      return;
    }
    if (split.rules.length >= MAX_SPLIT_RULES) {
      setSplitError(`You can exclude up to ${MAX_SPLIT_RULES} destinations.`);
      return;
    }
    const overlap = findOverlap(split.rules, parsed.value);
    if (overlap) {
      setSplitError(
        `${parsed.value.cidr} overlaps ${overlap.cidr}, which is already excluded.`,
      );
      return;
    }
    engine.setSplitTunnel({
      ...split,
      enabled: true,
      rules: [
        ...split.rules,
        { id: newRuleId(), cidr: parsed.value.cidr, label: sanitizeLabel(label) },
      ],
    });
    setSplitInput("");
    setSplitLabel("");
    setSplitError(null);
    if (parsed.value.normalized) notify(`Saved as ${parsed.value.cidr}`);
  };

  const addSplitPreset = (preset: SplitTunnelPreset) => {
    const def = SPLIT_TUNNEL_PRESETS[preset];
    const candidates = presetRules(preset);
    const next: SplitTunnelRule[] = [...split.rules];
    for (const rule of candidates) {
      if (next.length >= MAX_SPLIT_RULES) break;
      if (next.some((existing) => existing.cidr === rule.cidr)) continue;
      const parsed = parseCidr(rule.cidr);
      if (!parsed.ok) continue;
      if (findOverlap(next, parsed.value)) continue;
      next.push(rule);
    }
    const added = next.length - split.rules.length;
    if (added === 0) {
      setSplitError(`${def.label} is already covered by your exclusions.`);
      return;
    }
    engine.setSplitTunnel({ ...split, enabled: true, rules: next });
    setSplitError(null);
    notify(
      added === candidates.length
        ? `${def.label} added`
        : `Added ${added} of ${candidates.length} ranges`,
    );
  };

  const removeSplitRule = (id: string) => {
    engine.setSplitTunnel({
      ...split,
      rules: split.rules.filter((rule) => rule.id !== id),
    });
    setSplitError(null);
  };

  const submitSplit = () => {
    if (splitInput.trim()) addSplitRule(splitInput, splitLabel);
  };

  return (
    <div className="settings">
      <header className="page-head">
        <div>
          <h1>Settings</h1>
          <p>Plain-language controls. Advanced options live in the profile.</p>
        </div>
      </header>

      <div className="settings-scroll">
        <Section
          title="Account"
          note="Signing in runs Eddie's own login step once and saves the session keys in Nimbus's profile. Your password is kept in your system keyring so Nimbus can reconnect after a restart. Nimbus clears it from Eddie's profile when the engine stops."
        >
          <Field title="AirVPN login" desc="Your AirVPN username (not your email address).">
            <input
              className="text-input"
              type="text"
              value={engine.login}
              onChange={(e) => engine.setLogin(e.target.value)}
              placeholder="your-username"
              autoComplete="username"
              spellCheck={false}
              disabled={engine.signingIn}
            />
          </Field>
          <Field
            title="Password"
            desc="Saved in your system keyring after signing in. Nimbus uses it only to authorize connections and clears it from Eddie's profile on shutdown."
          >
            <input
              className="text-input"
              type="password"
              value={engine.password}
              onChange={(e) => engine.setPassword(e.target.value)}
              placeholder="••••••••••••"
              autoComplete="current-password"
              disabled={engine.signingIn}
            />
          </Field>
          <Field
            title="Sign in"
            desc={
              engine.mode === "live"
                ? engine.signingIn
                  ? "Contacting AirVPN and saving your session keys. This usually takes a few seconds."
                  : engine.signedIn
                    ? "Signed in. Nimbus restores this session from your system keyring on launch."
                    : "Sign in once. Nimbus remembers you in your system keyring, so later launches start signed in."
                : "Preview mode — the desktop app is needed to sign in."
            }
          >
            <div className="inline-actions">
              <Button
                variant="quiet"
                onClick={() => void engine.signIn()}
                disabled={engine.signingIn || engine.mode !== "live"}
                loading={engine.signingIn}
              >
                {engine.signingIn ? (
                  <>
                    <Spinner />
                    Signing in…
                  </>
                ) : engine.signedIn ? (
                  "Sign in again"
                ) : (
                  "Sign in"
                )}
              </Button>
              {engine.signedIn && engine.mode === "live" && (
                <Button
                  variant="quiet"
                  onClick={() => void engine.forgetSignIn()}
                  disabled={engine.signingIn}
                  title="Remove the saved password from your system keyring"
                >
                  Forget saved password
                </Button>
              )}
            </div>
          </Field>
        </Section>

        <Section title="Appearance">
          <Field title="Theme" desc="Switch any time with the button in the top-right corner.">
            <div className="segmented" role="group" aria-label="Theme">
              {(["dark", "light"] as const).map((t) => (
                <button
                  key={t}
                  type="button"
                  className={appearance.theme === t ? "is-active" : ""}
                  aria-pressed={appearance.theme === t}
                  onClick={() => appearance.setTheme(t)}
                >
                  {t === "dark" ? "Dark" : "Light"}
                </button>
              ))}
            </div>
          </Field>
          <Field title="Accent colour" desc="Used for the connect control, highlights and focus rings.">
            <div className="swatches" role="group" aria-label="Accent colour">
              {ACCENTS.map((a) => (
                <button
                  key={a.id}
                  type="button"
                  className={`swatch${appearance.accent === a.value ? " is-active" : ""}`}
                  style={{ background: a.value }}
                  aria-label={a.name}
                  aria-pressed={appearance.accent === a.value}
                  title={a.name}
                  onClick={() => appearance.setAccent(a.value)}
                >
                  {appearance.accent === a.value && <Check size={12} />}
                </button>
              ))}
            </div>
          </Field>
        </Section>

        <Section
          title="Connection"
          note="Changes apply the next time the tunnel is established."
        >
          <Field title="Protocol" desc="WireGuard is faster. OpenVPN is more widely supported.">
            <Select
              label="Protocol"
              value={engine.protocol}
              onChange={(v) => engine.setProtocol(v as Engine["protocol"])}
              options={[
                { value: "wireguard", label: "WireGuard" },
                { value: "openvpn", label: "OpenVPN" },
              ]}
            />
          </Field>
          <Field title="Port" desc="443 blends in with normal HTTPS traffic.">
            <Select
              label="Port"
              value={String(engine.port)}
              onChange={(v) => engine.setPort(Number(v))}
              options={[
                { value: "443", label: "443 / UDP" },
                { value: "1194", label: "1194 / UDP" },
                { value: "80", label: "80 / TCP" },
                { value: "53", label: "53 / UDP" },
              ]}
            />
          </Field>
          <Field
            title="Tunnel mode"
            desc="Route OpenVPN through SSL, SSH or Tor when the network blocks VPNs."
          >
            <Select
              label="Tunnel mode"
              value={mode}
              onChange={setMode}
              options={[
                { value: "direct", label: "Direct" },
                { value: "ssl", label: "SSL tunnel" },
                { value: "ssh", label: "SSH tunnel" },
                { value: "tor", label: "Tor" },
              ]}
            />
          </Field>
        </Section>

        <Section
          title="Split tunneling"
          note="Eddie's own route manager installs these exclusions; Nimbus never edits routes or firewall rules. IPv4 only for now. Changes apply the next time the tunnel connects."
        >
          <Field
            title="Exclude destinations"
            desc="Send chosen IPv4 addresses and subnets through your normal connection instead of the VPN."
          >
            <Toggle
              label="Split tunneling"
              checked={split.enabled}
              onChange={(enabled) => {
                setSplitError(null);
                engine.setSplitTunnel({ ...split, enabled });
              }}
            />
          </Field>

          {split.enabled && (
            <div className="split-panel">
              <div className="split-presets" role="group" aria-label="Destination presets">
                {(Object.keys(SPLIT_TUNNEL_PRESETS) as SplitTunnelPreset[]).map((id) => (
                  <button
                    key={id}
                    type="button"
                    className="chip"
                    title={SPLIT_TUNNEL_PRESETS[id].description}
                    onClick={() => addSplitPreset(id)}
                  >
                    {SPLIT_TUNNEL_PRESETS[id].label}
                  </button>
                ))}
              </div>

              {split.rules.length > 0 ? (
                <ul className="split-rules">
                  {split.rules.map((rule) => (
                    <li key={rule.id} className="split-rule">
                      <span className="split-cidr mono">{rule.cidr}</span>
                      <span className="split-rule-label">
                        {rule.label || presetLabel(rule.preset) || "Excluded"}
                      </span>
                      <button
                        type="button"
                        className="split-remove"
                        aria-label={`Remove ${rule.cidr}`}
                        title="Remove"
                        onClick={() => removeSplitRule(rule.id)}
                      >
                        <X size={13} />
                      </button>
                    </li>
                  ))}
                </ul>
              ) : (
                <p className="split-empty">
                  No destinations yet. Add an address below or start with a preset.
                </p>
              )}

              <div className="split-add">
                <input
                  className="text-input split-input"
                  type="text"
                  value={splitInput}
                  placeholder="192.168.0.0/16 or 203.0.113.7"
                  aria-label="Destination to exclude"
                  spellCheck={false}
                  maxLength={32}
                  onChange={(e) => {
                    setSplitInput(e.target.value);
                    if (splitError) setSplitError(null);
                  }}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      submitSplit();
                    }
                  }}
                />
                <input
                  className="text-input split-input split-input-label"
                  type="text"
                  value={splitLabel}
                  placeholder="Label (optional)"
                  aria-label="Label for this destination"
                  maxLength={48}
                  onChange={(e) => setSplitLabel(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      submitSplit();
                    }
                  }}
                />
                <Button variant="quiet" onClick={submitSplit} disabled={!splitInput.trim()}>
                  Add
                </Button>
              </div>

              {splitError && (
                <p className="split-error" role="alert">
                  {splitError}
                </p>
              )}

              <p className="split-warning">
                Excluded traffic does not go through the VPN. Those destinations see your real IP
                address and your ISP can see the traffic. DNS still goes through the tunnel, so a
                hostname can resolve to an address that is not excluded — prefer numeric addresses
                or CIDRs.
              </p>
            </div>
          )}

          {splitPending && (
            <div className="split-pending">
              <span>
                {split.enabled
                  ? "Applies on your next connection."
                  : "Split tunneling turns off on your next connection."}
              </span>
              <Button variant="quiet" onClick={engine.reconnect}>
                Reconnect
              </Button>
            </div>
          )}
        </Section>

        <Section
          title="App split tunneling"
          note="Choose which applications use the VPN, and see what the helper is steering."
        >
          <div className="apps-links">
            <Button variant="quiet" onClick={onOpenApps}>
              Open Apps
            </Button>
          </div>
        </Section>

        <Section
          title="Network protection"
          note="Network lock is enforced by Eddie's privileged helper. This client never touches firewall rules itself."
        >
          <Field
            title="Network lock"
            desc={
              engine.netlockAvailable
                ? "Block all traffic if the tunnel drops. Recommended."
                : "Temporarily disabled while the tunnel is validated. Eddie cannot release the lock over its JSON API, so a failed connect could otherwise cut your whole network."
            }
          >
            <Toggle
              label="Network lock"
              checked={engine.netlockAvailable && engine.netlock}
              disabled={!engine.netlockAvailable}
              onChange={engine.setNetlock}
            />
          </Field>
          <Field
            title="Release network lock"
            desc="Starts a short recovery engine that removes firewall rules left behind by an interrupted connection. Use it if the internet stays blocked after a failed connect."
          >
            <Button
              variant="quiet"
              disabled={engine.releasingLock || engine.connected || engine.state === "connecting"}
              onClick={async () => notify(await engine.releaseNetlock())}
            >
              {engine.releasingLock ? "Releasing…" : "Release lock"}
            </Button>
          </Field>
          <Field
            title="Block DNS outside the tunnel"
            desc="Prevents your ISP from seeing which sites you resolve."
          >
            <Toggle label="Block DNS leaks" checked={dnsBlock} onChange={setDnsBlock} />
          </Field>
          <Field
            title="Block IPv6"
            desc="Avoids leaks on networks that hand out IPv6 addresses."
          >
            <Toggle label="Block IPv6" checked={blockV6} onChange={setBlockV6} />
          </Field>
          <Field
            title="Reconnect automatically"
            desc="Re-establish the tunnel after a network change or sleep."
          >
            <Toggle label="Auto reconnect" checked={autoReconnect} onChange={setAutoReconnect} />
          </Field>
        </Section>

        <Section title="App">
          <Field title="Connect on launch" desc="Start the tunnel as soon as the app opens.">
            <Toggle label="Connect on launch" checked={autoConnect} onChange={setAutoConnect} />
          </Field>
          <Field title="Launch at login" desc="Start minimized to the tray.">
            <Toggle label="Launch at login" checked={launchAtLogin} onChange={setLaunchAtLogin} />
          </Field>
          <Field title="Notifications" desc="Tell me when the connection drops or recovers.">
            <Toggle label="Notifications" checked={notifications} onChange={setNotifications} />
          </Field>
        </Section>

        <Section title="About">
          <Field
            title="Version"
            desc={`Nimbus ${updates.currentVersion} · unofficial UI · last checked ${checkedLabel}`}
          >
            <Button
              variant="quiet"
              onClick={() => void updates.check(true)}
              loading={updates.checking}
              disabled={!updates.enabled || updates.checking || updates.installing}
            >
              {updates.checking ? "Checking…" : "Check for updates"}
            </Button>
          </Field>
          <Field
            title="Check on startup"
            desc="Look for a signed Nimbus release once when Nimbus starts. Nothing is downloaded or installed until you approve it."
          >
            <Toggle
              label="Check for updates on startup"
              checked={updates.autoCheck}
              onChange={updates.setAutoCheck}
              disabled={!updates.enabled || updates.installing}
            />
          </Field>

          {updates.info && (
            <div className="update-panel">
              <div className="update-copy">
                <div className="update-title">
                  Nimbus {updates.info.version} is available
                  <span className="badge">signed</span>
                </div>
                <p className="update-desc">
                  {!updates.supported
                    ? "This .deb build is updated through your package manager. Download the latest .deb or AppImage from GitHub Releases."
                    : engine.connected || engine.state === "connecting"
                      ? "Disconnect the tunnel before installing. Nimbus will replace this build and restart."
                      : "Nimbus will download the update, verify its signature, replace this build, and restart."}
                </p>
                {updates.info.body && <p className="update-notes">{updates.info.body}</p>}
                {updates.installing && (
                  <div className="update-progress-wrap">
                    <div
                      className="update-progress"
                      role="progressbar"
                      aria-valuenow={updates.progress ?? undefined}
                      aria-valuemin={0}
                      aria-valuemax={100}
                    >
                      <span style={{ width: `${updates.progress ?? 6}%` }} />
                    </div>
                    <span className="update-progress-label">
                      {updates.progress === null ? "Downloading…" : `${updates.progress}%`}
                    </span>
                  </div>
                )}
              </div>
              <div className="update-actions">
                {updates.supported ? (
                  <Button
                    variant="primary"
                    onClick={() => void installUpdate()}
                    loading={updates.installing}
                    disabled={
                      updates.installing ||
                      engine.connected ||
                      engine.state === "connecting" ||
                      engine.state === "disconnecting"
                    }
                  >
                    {updates.installing ? "Updating…" : "Update & restart"}
                  </Button>
                ) : (
                  <button
                    type="button"
                    className="btn btn-quiet"
                    onClick={() =>
                      notify("Download the latest build from github.com/coffeecode3d/nimbus/releases")
                    }
                  >
                    Where to download
                  </button>
                )}
              </div>
            </div>
          )}

          {updates.error && (
            <div className="update-panel is-error">
              <div className="update-copy">
                <div className="update-title">Update check failed</div>
                <p className="update-desc">{updates.error}</p>
              </div>
              <div className="update-actions">
                <button type="button" className="btn btn-quiet" onClick={updates.clearError}>
                  Dismiss
                </button>
              </div>
            </div>
          )}

          {!updates.info && !updates.error && updates.upToDate && (
            <div className="update-panel is-current">
              <div className="update-copy">
                <div className="update-title">Nimbus is up to date</div>
                <p className="update-desc">
                  You are running the newest published version. Nimbus will check again the
                  next time it starts.
                </p>
              </div>
            </div>
          )}

          <Field title="Engine" desc="Official Eddie CLI, driven over jsoninout.">
            <span className="engine-path mono">{engine.info?.cli ?? "Preview (simulated engine)"}</span>
          </Field>
          <Field
            title="Profile"
            desc="Nimbus keeps its own Eddie profile and never writes to Eddie's."
          >
            <span className="engine-path mono">{engine.info?.profile ?? "—"}</span>
          </Field>
          <Field
            title="Unofficial client"
            desc="Nimbus is not affiliated with or endorsed by AirVPN. Eddie is GPL-3.0."
          >
            <span className="badge">GPL-3.0</span>
          </Field>
        </Section>

        <p className="settings-foot">
          {engine.mode === "live"
            ? "Driving the official Eddie engine. The privileged helper and integrity checks are Eddie's, untouched."
            : "Preview mode uses a simulated engine. Launch the desktop app to drive the real Eddie engine."}
        </p>
      </div>
    </div>
  );
}
