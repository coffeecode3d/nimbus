import type { Engine } from "../engine";
import { formatDuration, formatRate } from "../engine";
import { Button, LoadBar, Sparkline, Toggle, Spinner } from "../components";
import { ArrowDown, ArrowUp, ChevronRight, Clock, Copy, Power, Shield } from "../icons";

const COPY: Record<
  Engine["state"],
  { eyebrow: string; title: string; sub: string; tone: string }
> = {
  disconnected: {
    eyebrow: "Not connected",
    title: "Your traffic is exposed",
    sub: "Connect to route everything through AirVPN.",
    tone: "idle",
  },
  connecting: {
    eyebrow: "Connecting",
    title: "Establishing tunnel",
    sub: "Negotiating keys and installing routes.",
    tone: "warn",
  },
  connected: {
    eyebrow: "Protected",
    title: "Connected",
    sub: "All traffic is encrypted and tunneled.",
    tone: "ok",
  },
  disconnecting: {
    eyebrow: "Disconnecting",
    title: "Closing tunnel",
    sub: "Restoring your default route.",
    tone: "warn",
  },
  blocked: {
    eyebrow: "Blocked",
    title: "Network locked",
    sub: "Traffic is blocked until the tunnel is up.",
    tone: "bad",
  },
};

export function ConnectScreen({
  engine,
  onOpenServers,
  notify,
}: {
  engine: Engine;
  onOpenServers: () => void;
  notify: (msg: string) => void;
}) {
  const { state, server, stats, connected, netlock, netlockAvailable, protocol, port } = engine;
  const copy = COPY[state];
  const busy = state === "connecting" || state === "disconnecting";
  const unavailable = engine.mode === "live" && (engine.status === "absent" || engine.status === "error");
  const engineMessage = engine.error ?? engine.info?.error ?? null;
  // Split tunneling makes "everything is tunneled" false. Say how many
  // destinations the active session excludes instead, and preview the count
  // while disconnected.
  const splitCount = engine.splitTunnel.enabled ? engine.splitTunnel.rules.length : 0;
  const appliedSplitCount = engine.splitTunnelApplied
    ? engine.splitTunnelApplied.split(";").filter(Boolean).length
    : 0;
  // App include mode inverts the question: the tunnel carries *only* these
  // apps, so "protected" applies to them and not to the rest of the desktop.
  const appliedApps = engine.appSplitApplied ?? [];
  const selectedApps = engine.appSplit.enabled ? engine.appSplit.apps : [];
  const plural = (n: number) => (n === 1 ? "" : "s");
  const appSub =
    state === "connected" && appliedApps.length > 0
      ? `Only ${appliedApps.length} selected app${plural(appliedApps.length)} ${
          appliedApps.length === 1 ? "uses" : "use"
        } the VPN; everything else uses your normal connection.`
      : state === "disconnected" && selectedApps.length > 0
        ? `Only ${selectedApps.length} selected app${plural(selectedApps.length)} will use the VPN.`
        : null;
  const sub =
    appSub ??
    (state === "connected" && appliedSplitCount > 0
      ? `${appliedSplitCount} destination${
          appliedSplitCount === 1 ? "" : "s"
        } bypass the VPN and are not protected by it.`
      : state === "disconnected" && splitCount > 0
        ? `${splitCount} destination${splitCount === 1 ? "" : "s"} will stay outside the VPN.`
        : copy.sub);

  const copyIp = async () => {
    try {
      await navigator.clipboard.writeText(server.ip);
      notify("Server IP copied");
    } catch {
      notify(server.ip);
    }
  };

  return (
    <div className="connect">
      <header className="status-head">
        <div className={`eyebrow eyebrow-${copy.tone}`}>
          <span className="eyebrow-dot" />
          {copy.eyebrow}
        </div>
        <h1>{copy.title}</h1>
        <p>{sub}</p>
      </header>

      {engine.mode === "live" && (engineMessage || unavailable) && (
        <div className={`engine-banner${unavailable ? " is-bad" : ""}`} role="alert">
          <div className="engine-banner-text">
            <strong>
              {engine.status === "absent"
                ? "Eddie engine not found"
                : unavailable
                  ? "Engine problem"
                  : "Engine message"}
            </strong>
            <span>{engineMessage ?? "eddie-cli is missing or not executable."}</span>
          </div>
          <div className="engine-banner-actions">
            <button type="button" className="btn btn-quiet" onClick={engine.retry}>
              Retry
            </button>
            <button
              type="button"
              className="engine-dismiss"
              onClick={engine.clearError}
              aria-label="Dismiss message"
            >
              ×
            </button>
          </div>
        </div>
      )}

      <button
        type="button"
        className={`orb orb-${state}`}
        onClick={engine.toggle}
        disabled={unavailable}
        aria-label={connected ? "Disconnect" : "Connect"}
      >
        <span className="orb-inner">
          <Power size={34} />
          <span className="orb-caption">
            {connected
              ? "Disconnect"
              : state === "connecting"
                ? "Connecting"
                : state === "disconnecting"
                  ? "Disconnecting"
                  : "Connect"}
          </span>
        </span>
      </button>

      <div className="primary-row">
        <Button
          variant={connected ? "ghost" : "primary"}
          wide
          onClick={engine.toggle}
          disabled={busy || unavailable}
          loading={busy}
        >
          {connected ? (
            "Disconnect"
          ) : state === "connecting" ? (
            <>
              <Spinner />
              Connecting…
            </>
          ) : state === "disconnecting" ? (
            "Disconnecting…"
          ) : (
            "Connect"
          )}
        </Button>
      </div>

      <div className="location-line">
        <button type="button" className="location" onClick={onOpenServers}>
          <span className="cc">{server.cc}</span>
          <span className="location-name">{server.name}</span>
          <span className="location-city">
            {server.city}, {server.country}
          </span>
          <ChevronRight size={16} className="location-chev" />
        </button>
      </div>

      <div className="meta-row">
        <button type="button" className="meta-item mono" onClick={copyIp}>
          {server.ip}
          <Copy size={13} />
        </button>
        <span className="meta-dot" />
        <span className="meta-item">{protocol === "wireguard" ? "WireGuard" : "OpenVPN"}</span>
        <span className="meta-dot" />
        <span className="meta-item">{port}/UDP</span>
      </div>

      <div className={`stats ${connected ? "is-live" : ""}`}>
        {connected ? (
          <Sparkline a={stats.samples} b={stats.upSamples} />
        ) : (
          <div className="stats-empty">Live throughput appears once connected</div>
        )}
        <div className="stats-row">
          <div className="stat">
            <ArrowDown size={14} />
            <span className="stat-value">{connected ? formatRate(stats.down) : "—"}</span>
          </div>
          <div className="stat">
            <ArrowUp size={14} />
            <span className="stat-value">{connected ? formatRate(stats.up) : "—"}</span>
          </div>
          <div className="stat stat-right">
            <Clock size={14} />
            <span className="stat-value">{connected ? formatDuration(stats.seconds) : "—"}</span>
          </div>
        </div>
      </div>

      <div className="tiles">
        <div className="tile">
          <Shield size={18} className="tile-icon" />
          <div className="tile-text">
            <div className="tile-title">Network lock</div>
            <div className="tile-desc">
              {netlockAvailable
                ? netlock
                  ? "Blocking leaks"
                  : "Off — traffic can leak"
                : "Off — disabled in this build"}
            </div>
          </div>
          <Toggle
            label="Network lock"
            checked={netlockAvailable && netlock}
            disabled={!netlockAvailable}
            onChange={engine.setNetlock}
          />
        </div>

        <button type="button" className="tile tile-button" onClick={onOpenServers}>
          <span className="tile-icon load-ring" title={`${server.load}% load`}>
            <LoadBar load={server.load} />
          </span>
          <span className="tile-text">
            <span className="tile-title">Server load</span>
            <span className="tile-desc">{server.load}% · {server.users} users</span>
          </span>
          <ChevronRight size={16} className="tile-chev" />
        </button>
      </div>
    </div>
  );
}
