import { useCallback, useEffect, useRef, useState } from "react";
import type {
  ConnState,
  EngineApi,
  EngineInfo,
  EngineStatus,
  Protocol,
  ServerInfo,
  Stats,
} from "./types";
import { isTauri, useLiveEngine } from "./liveEngine";

const SAMPLES = 56;

const clamp = (n: number, min: number, max: number) => Math.min(max, Math.max(min, n));
const round = (n: number, p = 1) => Math.round(n * 10 ** p) / 10 ** p;

function seedSamples(base: number, spread: number, count: number): number[] {
  return Array.from({ length: count }, (_, i) => {
    const wave = Math.sin(i / 3.1) * spread * 0.45 + Math.sin(i / 7.7) * spread * 0.3;
    return clamp(base + wave + (Math.random() - 0.5) * spread * 0.6, 0.2, base + spread);
  });
}

function initial(state: ConnState): Stats {
  if (state === "connected") {
    const down = seedSamples(13.4, 3.2, SAMPLES);
    return {
      down: round(down[down.length - 1] ?? 0),
      up: 2.7,
      seconds: 42 * 60 + 19,
      samples: down,
      upSamples: seedSamples(2.7, 0.9, SAMPLES),
    };
  }
  return {
    down: 0,
    up: 0,
    seconds: 0,
    samples: new Array(SAMPLES).fill(0),
    upSamples: new Array(SAMPLES).fill(0),
  };
}

function initialConn(): ConnState {
  if (typeof window === "undefined") return "disconnected";
  const q = new URLSearchParams(window.location.search).get("state");
  if (q === "connected" || q === "connecting" || q === "blocked" || q === "disconnecting") {
    return q;
  }
  return "disconnected";
}

/**
 * Deterministic placeholder latency, used only when the app runs in a browser
 * without the Eddie engine (mock mode).
 */
function mockLatency(s: ServerInfo): number {
  let h = 7;
  for (const c of s.id) h = (h * 31 + c.charCodeAt(0)) % 991;
  const base =
    s.continent === "Europe"
      ? 11
      : s.continent === "America"
        ? 92
        : s.continent === "Asia"
          ? 196
          : s.continent === "Africa"
            ? 158
            : 244;
  return base + (h % 42);
}

/**
 * Simulated engine for browser previews (`vite dev`, screenshots, docs).
 * It mirrors the live adapter's surface so screens never branch on the mode.
 */
export function useMockEngine(servers: ServerInfo[], enabled = true): EngineApi {
  const [state, setState] = useState<ConnState>(initialConn);
  const [serverId, setServerId] = useState(servers[0]?.id ?? "");
  const [netlock, setNetlock] = useState(false);
  const [protocol, setProtocol] = useState<Protocol>("wireguard");
  const [port, setPort] = useState(443);
  const [stats, setStats] = useState<Stats>(() => initial(initialConn()));
  const [error, setError] = useState<string | null>(null);
  const [login, setLogin] = useState("");
  const [password, setPassword] = useState("");
  const [favorites, setFavorites] = useState<string[]>(() => {
    try {
      return JSON.parse(localStorage.getItem("nimbus.favorites") ?? "[]");
    } catch {
      return [];
    }
  });
  const [recent, setRecent] = useState<string[]>(() => {
    try {
      return JSON.parse(localStorage.getItem("nimbus.recent") ?? "[]");
    } catch {
      return [];
    }
  });

  const timer = useRef<number | null>(null);

  useEffect(() => {
    localStorage.setItem("nimbus.favorites", JSON.stringify(favorites));
  }, [favorites]);

  useEffect(() => {
    localStorage.setItem("nimbus.recent", JSON.stringify(recent));
  }, [recent]);

  useEffect(() => {
    return () => {
      if (timer.current !== null) window.clearTimeout(timer.current);
    };
  }, []);

  // Simulated throughput sampling.
  useEffect(() => {
    if (!enabled || state !== "connected") return;
    const id = window.setInterval(() => {
      setStats((s) => {
        const down = clamp(s.down + (Math.random() - 0.5) * 2.4, 4, 26);
        const up = clamp(s.up + (Math.random() - 0.5) * 0.6, 0.6, 6);
        return {
          down: round(down),
          up: round(up),
          seconds: s.seconds + 1,
          samples: [...s.samples.slice(1), down],
          upSamples: [...s.upSamples.slice(1), up],
        };
      });
    }, 1000);
    return () => window.clearInterval(id);
  }, [enabled, state]);

  const connected = state === "connected";
  const server = servers.find((s) => s.id === serverId) ?? servers[0];

  const connect = useCallback(() => {
    if (state === "connecting" || state === "connected") return;
    setState("connecting");
    if (timer.current !== null) window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => {
      setState("connected");
      setStats(initial("connected"));
      setRecent((r) => [serverId, ...r.filter((x) => x !== serverId)].slice(0, 5));
    }, 1500);
  }, [state, serverId]);

  const disconnect = useCallback(() => {
    if (state === "disconnected" || state === "disconnecting" || state === "blocked") return;
    setState("disconnecting");
    if (timer.current !== null) window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => {
      setState("disconnected");
      setStats(initial("disconnected"));
    }, 550);
  }, [state]);

  const toggle = useCallback(() => {
    if (state === "connected" || state === "connecting") disconnect();
    else connect();
  }, [state, connect, disconnect]);

  const selectServer = useCallback(
    (id: string) => {
      setServerId(id);
      if (state === "connected" || state === "connecting") {
        setState("connecting");
        if (timer.current !== null) window.clearTimeout(timer.current);
        timer.current = window.setTimeout(() => {
          setState("connected");
          setStats(initial("connected"));
          setRecent((r) => [id, ...r.filter((x) => x !== id)].slice(0, 5));
        }, 900);
      }
    },
    [state],
  );

  const toggleFavorite = useCallback((id: string) => {
    setFavorites((f) => (f.includes(id) ? f.filter((x) => x !== id) : [...f, id]));
  }, []);

  const mode: "live" | "mock" = "mock";
  const status: EngineStatus = "ready";
  const info: EngineInfo | null = null;

  return {
    mode,
    state,
    server,
    serverId,
    servers,
    netlock,
    netlockAvailable: false,
    setNetlock,
    releaseNetlock: async () => "Network lock released.",
    releasingLock: false,
    protocol,
    setProtocol,
    port,
    setPort,
    stats,
    favorites,
    recent,
    toggleFavorite,
    connect,
    disconnect,
    toggle,
    selectServer,
    connected,
    // Engine surface (no-ops in preview mode).
    status,
    info,
    error,
    clearError: () => setError(null),
    retry: () => setError(null),
    ready: true,
    login,
    setLogin,
    password,
    setPassword,
    signIn: async () => {
      setError("The real engine is only available in the desktop app.");
    },
    signingIn: false,
    signedIn: false,
    forgetSignIn: async () => {},
    latencyFor: (s: ServerInfo): number | undefined => mockLatency(s),
    refreshLatency: async () => {},
    measuringLatency: false,
    installEngine: async () => {
      setError("The real engine is only available in the desktop app.");
    },
    installing: false,
    installStage: null,
    installMessage: null,
    installProgress: null,
    installError: null,
    refreshEngine: async () => {},
  };
}

export type Engine = EngineApi;

/**
 * One entry point for screens. Inside Tauri this drives the official engine;
 * in a plain browser it falls back to the simulator.
 */
export function useEngine(servers: ServerInfo[]): Engine {
  const live = isTauri();
  const mock = useMockEngine(servers, !live);
  const real = useLiveEngine(servers, live);
  return live ? real : mock;
}

export function formatDuration(seconds: number): string {
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = seconds % 60;
  return [h, m, s].map((n) => String(n).padStart(2, "0")).join(":");
}

export function formatRate(mbps: number): string {
  if (mbps < 0.1) return "0 B/s";
  if (mbps < 10) return `${mbps.toFixed(1)} MB/s`;
  return `${Math.round(mbps)} MB/s`;
}
