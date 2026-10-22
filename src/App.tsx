import { useCallback, useEffect, useRef, useState } from "react";
import { useEngine } from "./engine";
import { useAppearance } from "./appearance";
import type { View } from "./types";
import serversJson from "./servers.json";
import type { ServerInfo } from "./types";
import { BrandMark, ChevronRight, Globe, Moon, Power, Sliders, Sun } from "./icons";
import { ConnectScreen } from "./screens/Connect";
import { ServersScreen } from "./screens/Servers";
import { SettingsScreen } from "./screens/Settings";

const servers = serversJson as ServerInfo[];

const NAV: { id: View; label: string; icon: typeof Power }[] = [
  { id: "connect", label: "Connect", icon: Power },
  { id: "servers", label: "Servers", icon: Globe },
  { id: "settings", label: "Settings", icon: Sliders },
];

function accountInitials(login: string, mode: "live" | "mock"): string {
  if (!login) return mode === "live" ? "?" : "PV";
  const name = login.split("@")[0].replace(/[^a-zA-Z0-9]/g, "");
  return name.slice(0, 2).toUpperCase() || "?";
}

export function App() {
  const engine = useEngine(servers);
  const appearance = useAppearance();
  const [view, setView] = useState<View>(() => {
    const q = new URLSearchParams(window.location.search).get("view");
    return q === "servers" || q === "settings" ? q : "connect";
  });
  const [toast, setToast] = useState<string | null>(null);
  const toastTimer = useRef<number | null>(null);

  const notify = useCallback((msg: string) => {
    setToast(msg);
    if (toastTimer.current !== null) window.clearTimeout(toastTimer.current);
    toastTimer.current = window.setTimeout(() => setToast(null), 2200);
  }, []);

  useEffect(() => {
    return () => {
      if (toastTimer.current !== null) window.clearTimeout(toastTimer.current);
    };
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "1" && (e.metaKey || e.ctrlKey)) setView("connect");
      if (e.key === "2" && (e.metaKey || e.ctrlKey)) setView("servers");
      if (e.key === "3" && (e.metaKey || e.ctrlKey)) setView("settings");
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div className="app">
      <aside className="rail">
        <div className="brand">
          <span className="brand-mark">
            <BrandMark size={22} />
          </span>
          <div className="brand-text">
            <div className="brand-name">Nimbus</div>
            <div className="brand-sub">for AirVPN</div>
          </div>
        </div>

        <nav className="nav" aria-label="Main">
          {NAV.map(({ id, label, icon: Icon }) => (
            <button
              key={id}
              type="button"
              className={`nav-item${view === id ? " is-active" : ""}`}
              onClick={() => setView(id)}
              aria-current={view === id ? "page" : undefined}
            >
              <Icon size={18} />
              <span className="nav-label">{label}</span>
              {id === "servers" && <span className="nav-count">{servers.length}</span>}
            </button>
          ))}
        </nav>

        <div className="rail-spacer" />

        <div className="rail-note">
          {engine.mode === "live" ? "Official engine" : "Prototype UI"}
          <br />
          {engine.mode === "live"
            ? engine.connected
              ? "tunnel up"
              : `engine ${engine.status}`
            : "simulated engine"}
        </div>

        <button type="button" className="account" onClick={() => setView("settings")}>
          <span className="avatar">{accountInitials(engine.login, engine.mode)}</span>
          <span className="account-text">
            <span className="account-name">
              {engine.login ? engine.login.split("@")[0] : engine.mode === "live" ? "Sign in" : "Previewer"}
            </span>
            <span className="account-sub">
              {engine.mode === "live"
                ? engine.connected
                  ? "Connected"
                  : "AirVPN account"
                : "Preview account"}
            </span>
          </span>
          <ChevronRight size={15} />
        </button>
      </aside>

      <main className="content">
        <div className="titlebar" data-tauri-drag-region>
          <button
            type="button"
            className="icon-btn no-drag"
            onClick={appearance.toggleTheme}
            aria-label={`Switch to ${appearance.theme === "dark" ? "light" : "dark"} mode`}
            title={`Switch to ${appearance.theme === "dark" ? "light" : "dark"} mode`}
          >
            {appearance.theme === "dark" ? <Sun size={15} /> : <Moon size={15} />}
          </button>
        </div>
        {view === "connect" && (
          <ConnectScreen engine={engine} onOpenServers={() => setView("servers")} notify={notify} />
        )}
        {view === "servers" && <ServersScreen engine={engine} notify={notify} />}
        {view === "settings" && (
          <SettingsScreen engine={engine} appearance={appearance} notify={notify} />
        )}
      </main>

      {toast && (
        <div className="toast" role="status">
          <span className="toast-dot" />
          {toast}
        </div>
      )}
    </div>
  );
}
