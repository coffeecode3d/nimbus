import { useState } from "react";
import type { Engine } from "../engine";
import { ACCENTS } from "../appearance";
import type { Appearance } from "../appearance";
import { Field, Section, Select, Toggle, Button, Spinner } from "../components";
import { Check } from "../icons";

export function SettingsScreen({
  engine,
  appearance,
  notify,
}: {
  engine: Engine;
  appearance: Appearance;
  notify: (msg: string) => void;
}) {
  const [dnsBlock, setDnsBlock] = useState(true);
  const [blockV6, setBlockV6] = useState(true);
  const [autoConnect, setAutoConnect] = useState(false);
  const [launchAtLogin, setLaunchAtLogin] = useState(false);
  const [notifications, setNotifications] = useState(true);
  const [autoReconnect, setAutoReconnect] = useState(true);
  const [mode, setMode] = useState("direct");

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
          note="Signing in runs Eddie's own login step once and saves the session keys in Nimbus's profile. Your password is kept in your system keyring so Nimbus can reconnect after a restart; it is never written to the profile."
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
            desc="Saved in your system keyring after signing in. It is never written to the Eddie profile, and Nimbus uses it only to authorize connections."
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
          <Field title="Version" desc="Nimbus 0.1.0 · unofficial UI">
            <button type="button" className="btn btn-quiet" onClick={() => notify("You're up to date")}>
              Check for updates
            </button>
          </Field>
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
