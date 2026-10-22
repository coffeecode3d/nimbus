import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  ConnState,
  EngineApi,
  EngineInfo,
  EngineStatus,
  Protocol,
  ServerInfo,
  Stats,
} from "./types";

const SAMPLES = 56;
const STATS_CALLBACK = "nimbus.stats";

export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export type { EngineInfo, EngineStatus };

interface StartResult {
  info: EngineInfo;
  session: string;
}

export interface StartOptions {
  server?: string;
  protocol?: string;
  mode?: string;
  port?: number;
  netlock?: boolean;
  /** Frontend-generated tag so early engine events are not dropped. */
  session?: string;
}

interface EngineLine {
  session: string;
  line: string;
}

interface InstallEvent {
  stage: string;
  message: string;
  progress: number | null;
}

/* ------------------------------------------------------------------ parsing */

export function parseRate(text: string | undefined): number {
  if (!text) return 0;
  const match = text.match(/([\d.]+)\s*([KMGTP]?)(i?)(B|bit)\s*\/?\s*s/i);
  if (!match) return 0;
  const value = Number(match[1]);
  if (!Number.isFinite(value)) return 0;
  const prefix = match[2].toUpperCase();
  const binary = match[3] === "i";
  const bits = /bit/i.test(match[4]);
  const scale: Record<string, number> = {
    "": 1e-6,
    K: binary ? 1.024e-3 : 1e-3,
    M: 1,
    G: 1024,
    T: 1024 * 1024,
  };
  const mb = (bits ? value / 8 : value) * (scale[prefix] ?? 1);
  return Number(mb.toFixed(2));
}

export function parseSeconds(text: string | undefined): number | null {
  if (!text) return null;
  const match = text.trim().match(/^(?:(\d+)\s+days?,?\s+)?(\d+):(\d{1,2})(?::(\d{1,2}))?$/i);
  if (!match) return null;
  if (match[4] !== undefined) {
    const days = Number(match[1] ?? 0);
    return days * 86400 + Number(match[2]) * 3600 + Number(match[3]) * 60 + Number(match[4]);
  }
  return Number(match[2]) * 60 + Number(match[3]);
}

const STATE_DIR_HELP =
  "Eddie's root helper is missing its state directory. Run: sudo install -d -o root -g root -m 700 /var/lib/eddie-vpn/state";

function friendlyEngineError(message: string): string {
  if (/state directory not available/i.test(message)) return STATE_DIR_HELP;
  if (/wrong login\/?password/i.test(message)) {
    return "Wrong AirVPN username or password. Sign in again from Settings → Account.";
  }
  return message;
}

const NOT_CONNECTED = /^not connected\.?$/i;

const emptyStats = (): Stats => ({
  down: 0,
  up: 0,
  seconds: 0,
  samples: new Array(SAMPLES).fill(0),
  upSamples: new Array(SAMPLES).fill(0),
});

/* ------------------------------------------------------------------- hooks */

/**
 * Drives the official `eddie-cli --jsoninout` engine.
 *
 * Only the engine is real here; all rendering stays in React. Commands are
 * written as JSON lines on stdin, events arrive on stdout and are re-emitted
 * by the Tauri side as `engine://line`.
 */
export function useLiveEngine(servers: ServerInfo[], enabled: boolean): EngineApi {
  const [state, setState] = useState<ConnState>("disconnected");
  const [serverId, setServerId] = useState(servers[0]?.id ?? "");
  const [netlock, setNetlock] = useState(false);
  const [protocol, setProtocol] = useState<Protocol>("wireguard");
  const [port, setPort] = useState(443);
  const [stats, setStats] = useState<Stats>(emptyStats);
  const [favorites, setFavorites] = useState<string[]>(() => {
    try {
      return JSON.parse(localStorage.getItem("nimbus.favorites") ?? '["toliman","vindemiatrix"]');
    } catch {
      return [];
    }
  });
  const [recent, setRecent] = useState<string[]>(() => {
    try {
      return JSON.parse(localStorage.getItem("nimbus.recent") ?? '["toliman"]');
    } catch {
      return [];
    }
  });

  const [status, setStatus] = useState<EngineStatus>("idle");
  const [info, setInfo] = useState<EngineInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [actualServerName, setActualServerName] = useState<string | null>(null);
  const [login, setLoginState] = useState(() => localStorage.getItem("nimbus.login") ?? "");
  const [password, setPasswordState] = useState("");
  const [signedIn, setSignedIn] = useState(false);
  const [signingIn, setSigningIn] = useState(false);
  const [latencies, setLatencies] = useState<Record<string, number>>({});
  const [measuringLatency, setMeasuringLatency] = useState(false);
  const [releasingLock, setReleasingLock] = useState(false);
  const [installing, setInstalling] = useState(false);
  const [installStage, setInstallStage] = useState<string | null>(null);
  const [installMessage, setInstallMessage] = useState<string | null>(null);
  const [installProgress, setInstallProgress] = useState<number | null>(null);
  const [installError, setInstallError] = useState<string | null>(null);

  // Network lock is disabled until the tunnel has been validated end to end.
  // Eddie only arms or disarms it during engine startup, and a session that
  // dies in the provider authorization call can leave the firewall armed with
  // no command available to turn it off again.
  const netlockAvailable = false;

  const session = useRef<string | null>(null);
  const readyRef = useRef(false);
  const wantedConnect = useRef(false);
  const connectSent = useRef(false);
  const startInFlight = useRef(false);
  const disarming = useRef(false);
  const watchdog = useRef<number | null>(null);
  const installInFlight = useRef(false);
  const abortRef = useRef<(message: string) => void>(() => undefined);
  const statusRef = useRef<ConnState>("disconnected");
  const optionsRef = useRef({ serverId, protocol, port, netlock });
  const connectTimer = useRef<number | null>(null);
  // Eddie's per-session `act=connect` authorization needs the account
  // credentials, so keep them in memory for this app session. They are handed
  // to Eddie over stdin and cleared from its profile on a clean shutdown.
  const credentialsRef = useRef<{ login: string; password: string } | null>(null);
  const pollTimer = useRef<number | null>(null);
  const mounted = useRef(true);
  const restoredCredentials = useRef(false);

  // Restore the saved account password from the OS keyring once per launch.
  // Eddie needs it for `act=connect`; keeping it out of the profile and out of
  // localStorage means only the system credential store holds the secret.
  useEffect(() => {
    if (!enabled || restoredCredentials.current) return;
    restoredCredentials.current = true;
    const account = (localStorage.getItem("nimbus.login") ?? "").trim();
    if (!account) return;
    // Do not cancel this on StrictMode's simulated unmount. The first effect
    // invocation starts the read and the second returns early because of
    // `restoredCredentials`, so a cancellation flag would discard the result.
    void invoke<string | null>("credentials_load", { login: account })
      .then((stored) => {
        if (!mounted.current || !stored) return;
        credentialsRef.current = { login: account, password: stored };
        setSignedIn(true);
      })
      .catch(() => {
        // A locked or unavailable keyring just means the user signs in again.
      });
  }, [enabled]);

  useEffect(() => {
    statusRef.current = state;
  }, [state]);

  useEffect(() => {
    optionsRef.current = { serverId, protocol, port, netlock };
  }, [serverId, protocol, port, netlock]);

  useEffect(() => {
    localStorage.setItem("nimbus.favorites", JSON.stringify(favorites));
  }, [favorites]);

  useEffect(() => {
    localStorage.setItem("nimbus.recent", JSON.stringify(recent));
  }, [recent]);

  const setLogin = useCallback((value: string) => {
    setLoginState(value);
    // Editing the account details means both the saved session and the
    // credentials held for this session may no longer match.
    credentialsRef.current = null;
    setSignedIn(false);
    if (value) localStorage.setItem("nimbus.login", value);
    else localStorage.removeItem("nimbus.login");
  }, []);

  const setPassword = useCallback((value: string) => {
    setPasswordState(value);
    credentialsRef.current = null;
    setSignedIn(false);
  }, []);

  const send = useCallback(async (payload: object) => {
    try {
      await invoke("engine_send", { line: JSON.stringify(payload) });
      return true;
    } catch {
      return false;
    }
  }, []);

  const requestStats = useCallback(() => {
    void send({ command: "stats.list", callback: STATS_CALLBACK });
  }, [send]);

  const serverCode = useCallback(
    (id: string) => servers.find((s) => s.id === id)?.name,
    [servers],
  );

  const start = useCallback(
    async (
      options: { serverId: string; protocol: Protocol; port: number; netlock: boolean },
      connect: boolean,
    ): Promise<boolean> => {
      // Avoid a double click spawning two engines: the second one would either
      // hit Eddie's single-instance guard or race the first one's session. The
      // call is accepted, not failed, so the caller keeps its connect intent.
      if (startInFlight.current) return true;
      startInFlight.current = true;
      if (!connect) wantedConnect.current = false;
      connectSent.current = false;
      setStatus("starting");
      setError(null);
      readyRef.current = false;
      // Tag this start before spawning. The engine can emit `engine.ready`
      // while `engine_start` is still resolving; without an id in advance the
      // listener would discard that event and the connect would hang.
      const pendingSession = `s${Date.now().toString(36)}${Math.random()
        .toString(36)
        .slice(2, 8)}`;
      // Trailing events from the process we are about to replace carry the
      // previous id, so they are ignored.
      session.current = pendingSession;
      if (connectTimer.current !== null) {
        window.clearTimeout(connectTimer.current);
        connectTimer.current = null;
      }
      if (watchdog.current !== null) {
        window.clearTimeout(watchdog.current);
        watchdog.current = null;
      }
      if (connect) {
        // Eddie has no request timeout on the provider authorization call.
        // Do not sit in "Connecting" forever if the API or the tunnel stalls.
        watchdog.current = window.setTimeout(() => {
          watchdog.current = null;
          if (statusRef.current !== "connected") {
            abortRef.current(
              "Eddie did not finish authorizing the session within 75 seconds. Check Settings → Engine for details, then try another server.",
            );
          }
        }, 75000);
      }

      const payload: StartOptions = {
        server: serverCode(options.serverId),
        protocol: options.protocol,
        mode: options.protocol === "openvpn" ? "udp" : undefined,
        port: options.port,
        // Network lock is off for every run in this build. Eddie cannot
        // deactivate it over jsoninout, and a failed connect that leaves the
        // firewall armed locks the whole machine off the network.
        netlock: false,
        session: pendingSession,
      };

      try {
        const result = await invoke<StartResult>("engine_start", { options: payload });
        session.current = result.session;
        setInfo(result.info);
        startInFlight.current = false;
        return true;
      } catch (err) {
        startInFlight.current = false;
        session.current = null;
        if (!mounted.current) return false;
        setStatus("error");
        setState("disconnected");
        setError(String(err));
        return false;
      }
    },
    [serverCode],
  );

  const abortConnect = useCallback((message: string) => {
    wantedConnect.current = false;
    if (connectTimer.current !== null) {
      window.clearTimeout(connectTimer.current);
      connectTimer.current = null;
    }
    if (watchdog.current !== null) {
      window.clearTimeout(watchdog.current);
      watchdog.current = null;
    }
    setError(message);
    setState("disconnected");
    setStats(emptyStats());
    setActualServerName(null);
    session.current = null;
    setStatus("stopped");
    // Eddie's jsoninout API has no netlock command. A throwaway engine with
    // the lock disabled makes Eddie load its recovery file and tear down any
    // firewall rules left behind by a session that never shut down cleanly.
    if (!disarming.current) {
      disarming.current = true;
      void invoke<{ ok: boolean; message: string }>("engine_disarm")
        .catch(() => undefined)
        .finally(() => {
          disarming.current = false;
        });
    }
  }, []);

  useEffect(() => {
    abortRef.current = abortConnect;
  }, [abortConnect]);

  const applyStatus = useCallback((obj: Record<string, unknown>) => {
    // Note: `netlock` in ui.main-status is the *active* lock state, not the
    // user's preference. Do not write it back to the toggle: with no session
    // running Eddie reports false and it would silently disable the setting.
    const action = String(obj.action_icon ?? "");
    if (action === "connected") {
      wantedConnect.current = false;
      if (connectTimer.current !== null) {
        window.clearTimeout(connectTimer.current);
        connectTimer.current = null;
      }
      if (watchdog.current !== null) {
        window.clearTimeout(watchdog.current);
        watchdog.current = null;
      }
      setError(null);
      setState("connected");
    } else if (action === "pending") {
      // The engine has credentials and is working on a session; the earlier
      // "no connection available" was just the pre-login state.
      if (connectTimer.current !== null) {
        window.clearTimeout(connectTimer.current);
        connectTimer.current = null;
      }
      setState((prev) =>
        prev === "connected" || prev === "disconnecting" ? "disconnecting" : "connecting",
      );
    } else if (action === "cantconnect") {
      if (statusRef.current !== "connected") setState("disconnected");
      if (wantedConnect.current && !connectTimer.current) {
        connectTimer.current = window.setTimeout(() => {
          connectTimer.current = null;
          if (statusRef.current !== "connected") {
            abortConnect(
              "Eddie could not start an AirVPN session. If you have not signed in yet, open Settings → Account and sign in; otherwise check the engine log there.",
            );
          }
        }, 12000);
      }
    } else {
      setState("disconnected");
    }
  }, [abortConnect]);

  const applyStats = useCallback(
    (list: Array<{ key: string; value: unknown }>) => {
      const map: Record<string, string> = {};
      for (const row of list) map[String(row.key)] = String(row.value ?? "");

      const down = parseRate(map.VpnSpeedDownload);
      const up = parseRate(map.VpnSpeedUpload);
      const seconds = parseSeconds(map.VpnStart);

      setStats((prev) => ({
        down,
        up,
        seconds: seconds ?? prev.seconds,
        samples: [...prev.samples.slice(1), down],
        upSamples: [...prev.upSamples.slice(1), up],
      }));

      const name = map.ServerName;
      if (name && !NOT_CONNECTED.test(name)) {
        setActualServerName(name);
      } else if (statusRef.current !== "connected") {
        setActualServerName(null);
      }
    },
    [],
  );

  const handleLine = useCallback(
    (line: string) => {
      let obj: Record<string, unknown>;
      try {
        obj = JSON.parse(line);
      } catch {
        return;
      }
      const command = String(obj.command ?? "");

      if (command === "engine.ready") {
        readyRef.current = true;
        setStatus("ready");
        requestStats();
        // The engine always starts idle so the password never has to appear
        // in its argv. Hand it the credentials over stdin, then connect.
        if (wantedConnect.current && !connectSent.current) {
          connectSent.current = true;
          const credentials = credentialsRef.current;
          if (!credentials) {
            abortConnect(
              "Sign in from Settings → Account before connecting. Eddie needs your password to authorize the session.",
            );
          } else {
            void (async () => {
              const ok =
                (await send({
                  command: "options.set",
                  name: "login",
                  value: credentials.login,
                })) &&
                (await send({
                  command: "options.set",
                  name: "password",
                  value: credentials.password,
                })) &&
                (await send({ command: "mainaction.connect" }));
              if (!ok) {
                abortConnect(
                  "The engine did not accept the connection request. Try again.",
                );
              }
            })();
          }
        }
      } else if (command === "engine.shutdown") {
        readyRef.current = false;
      } else if (command === "ui.main-status") {
        applyStatus(obj);
      } else if (command === "ui.stats.change") {
        // Keep only the keys we need for the stats card; the full list is
        // refreshed on the next poll.
        if (obj.key === "VpnStart" && typeof obj.value === "string") {
          const seconds = parseSeconds(obj.value);
          if (seconds !== null) setStats((prev) => ({ ...prev, seconds }));
        }
      } else if (command === "reply" && obj.id === STATS_CALLBACK) {
        const body = obj.body as { list?: Array<{ key: string; value: unknown }> } | null;
        if (body?.list) applyStats(body.list);
      } else if (
        command === "ui.notification" &&
        (obj.level === "error" || obj.level === "fatal")
      ) {
        const message = String(obj.message ?? "Eddie reported an error.");
        setError(friendlyEngineError(message));
      } else if (command === "log") {
        const type = String(obj.type ?? "");
        const message = String(obj.message ?? "");
        if (!message) return;
        if (/already running/i.test(message)) {
          abortConnect("Another Eddie engine is already running. Quit Eddie and try again.");
        } else if (/state directory not available/i.test(message)) {
          abortConnect(STATE_DIR_HELP);
        } else if (
          type === "fatal" ||
          // Be specific here: Eddie logs normal progress as
          // "Checking authorization ...", which an /auth/ match used to treat
          // as a failed login and abort the session after 300 ms.
          (/cannot login|login failed|wrong login|invalid .*(login|password|key)|not authorized|authentication failed|auth failed/i.test(
            message,
          ) &&
            wantedConnect.current)
        ) {
          const friendly = friendlyEngineError(message);
          if (wantedConnect.current) abortConnect(friendly);
          else setError(friendly);
        } else if (type === "error") {
          setError(friendlyEngineError(message));
        }
      }
    },
    [abortConnect, applyStats, applyStatus, requestStats, send],
  );

  /* --------------------------------------------------------------- lifecycle */

  useEffect(() => {
    if (!enabled) return;
    mounted.current = true;

    let disposed = false;
    const unlisten: Array<() => void> = [];

    void invoke<EngineInfo>("engine_info")
      .then((value) => {
        if (!disposed) {
          setInfo(value);
          setStatus(value.found ? "idle" : "absent");
          if (value.error) setError(value.error);
        }
      })
      .catch(() => {
        if (!disposed) setStatus("absent");
      });

    void (async () => {
      const line = await listen<EngineLine>("engine://line", (event) => {
        if (event.payload.session !== session.current) return;
        if (!disposed) handleLine(event.payload.line);
      });
      if (disposed) line();
      else unlisten.push(line);

      const eof = await listen<string>("engine://eof", (event) => {
        if (event.payload !== session.current) return;
        if (disposed) return;
        readyRef.current = false;
        if (watchdog.current !== null) {
          window.clearTimeout(watchdog.current);
          watchdog.current = null;
        }
        setStatus("stopped");
        setActualServerName(null);
        setState("disconnected");
        setStats(emptyStats());
      });
      if (disposed) eof();
      else unlisten.push(eof);

      const stderr = await listen<EngineLine>("engine://stderr", (event) => {
        if (event.payload.session !== session.current) return;
        const text = event.payload.line;
        if (/error|fail|denied|not allowed|integrity/i.test(text) && !disposed) {
          setError(text);
        }
      });
      if (disposed) stderr();
      else unlisten.push(stderr);
    })();

    return () => {
      disposed = true;
      mounted.current = false;
      for (const stop of unlisten) stop();
    };
  }, [enabled, handleLine]);

  // The installer streams progress from the Rust side. Listening once at mount
  // avoids missing the first event between the click and a per-call listener.
  useEffect(() => {
    if (!enabled) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen<InstallEvent>("engine://install", (event) => {
      if (disposed) return;
      const payload = event.payload;
      setInstallStage(payload.stage);
      setInstallMessage(payload.message);
      setInstallProgress(payload.progress);
      if (payload.stage === "error") setInstallError(payload.message);
    }).then((stop) => {
      if (disposed) stop();
      else unlisten = stop;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [enabled]);

  // Poll the engine's stats once per second while it is ready. `stats.list`
  // is the one command this Eddie build reliably answers over jsoninout.
  useEffect(() => {
    if (!enabled || status !== "ready") return;
    requestStats();
    pollTimer.current = window.setInterval(requestStats, 1000);
    return () => {
      if (pollTimer.current !== null) {
        window.clearInterval(pollTimer.current);
        pollTimer.current = null;
      }
    };
  }, [enabled, status, requestStats]);

  /* ----------------------------------------------------------------- actions */

  const connect = useCallback(async () => {
    if (state === "connecting" || state === "connected") return;
    // The saved profile is enough for the engine to know the account and its
    // keys, but Eddie's `act=connect` authorization still needs the password.
    if (!credentialsRef.current) {
      setError(
        "Sign in from Settings → Account before connecting. Eddie needs your password to authorize the session.",
      );
      return;
    }
    wantedConnect.current = true;
    setState("connecting");
    setError(null);
    const opts = optionsRef.current;
    const ok = await start(opts, true);
    if (!ok) {
      wantedConnect.current = false;
      setState("disconnected");
    }
  }, [state, start]);

  const disconnect = useCallback(async () => {
    if (state === "disconnected" || state === "disconnecting") return;
    wantedConnect.current = false;
    if (watchdog.current !== null) {
      window.clearTimeout(watchdog.current);
      watchdog.current = null;
    }
    setState("disconnecting");
    const sent = await send({ command: "mainaction.disconnect" });
    if (!sent) {
      setState("disconnected");
      return;
    }
    window.setTimeout(() => {
      if (statusRef.current === "disconnecting") {
        // The provider never acknowledged the disconnect. Stop the engine and
        // run the recovery pass so nothing half-armed is left behind.
        session.current = null;
        setStatus("stopped");
        setState("disconnected");
        void invoke("engine_disarm").catch(() => undefined);
      } else {
        setState((prev) => (prev === "disconnecting" ? "disconnected" : prev));
      }
    }, 6000);
  }, [state, send]);

  const stopForUpdate = useCallback(async () => {
    wantedConnect.current = false;
    if (watchdog.current !== null) {
      window.clearTimeout(watchdog.current);
      watchdog.current = null;
    }
    session.current = null;
    readyRef.current = false;
    setState("disconnected");
    setStatus("stopped");
    setActualServerName(null);
    setStats(emptyStats());
    try {
      await invoke("engine_stop");
    } catch {
      // The updater should still be allowed to proceed if the engine already
      // exited on its own.
    }
  }, []);

  const toggle = useCallback(() => {
    if (state === "connected" || state === "connecting") void disconnect();
    else void connect();
  }, [state, connect, disconnect]);

  const selectServer = useCallback(
    (id: string) => {
      // Update the ref immediately: callers often do selectServer(id) then
      // connect() in the same tick, before React commits the new state.
      optionsRef.current = { ...optionsRef.current, serverId: id };
      setServerId(id);
      if (state === "connected" || state === "connecting") {
        wantedConnect.current = true;
        setState("connecting");
        void start({ ...optionsRef.current, serverId: id }, true);
      }
    },
    [state, start],
  );

  const signIn = useCallback(async () => {
    const account = login.trim();
    if (!account || !password) {
      setError("Enter both your AirVPN login and password.");
      return;
    }
    setSigningIn(true);
    setError(null);
    try {
      // One short console-mode run performs Eddie's real login and saves the
      // authenticated user (keys, certificates) into Nimbus's profile. The
      // jsoninout engine has no login command of its own.
      const result = await invoke<{ ok: boolean; message: string }>("engine_authenticate", {
        login: account,
        password,
      });
      if (!result.ok) {
        setError(result.message);
        return;
      }
      // Keep the password in memory for this app session: `act=connect` needs
      // it for every session. Eddie may cache it in its encrypted profile
      // until shutdown, where the engine stop path clears it.
      credentialsRef.current = { login: account, password };
      setPasswordState("");
      setSignedIn(true);
      // Load the freshly saved profile in the UI engine.
      await start(optionsRef.current, false);
      // Persist the password in the OS keyring so the next launch does not
      // ask for it again. Signing in still succeeds if the keyring is locked.
      void invoke("credentials_save", { login: account, password }).catch((err) => {
        if (mounted.current) {
          setError(
            (previous) =>
              previous ??
              `Signed in for this session, but the password could not be saved to the system keyring: ${String(err)}`,
          );
        }
      });
    } catch (err) {
      setError(String(err));
    } finally {
      setSigningIn(false);
    }
  }, [login, password, start]);

  const forgetSignIn = useCallback(async () => {
    const account = credentialsRef.current?.login || login.trim();
    credentialsRef.current = null;
    setSignedIn(false);
    setPasswordState("");
    if (!account) return;
    try {
      await invoke("credentials_delete", { login: account });
    } catch (err) {
      setError(String(err));
    }
  }, [login]);

  const installEngine = useCallback(async () => {
    if (installInFlight.current) return;
    installInFlight.current = true;
    setInstalling(true);
    setInstallError(null);
    setInstallStage("starting");
    setInstallMessage("Preparing the official engine…");
    setInstallProgress(null);
    try {
      const value = await invoke<EngineInfo>("engine_install");
      if (!mounted.current) return;
      setInfo(value);
      setStatus(value.found ? "idle" : "absent");
      if (value.found) {
        setInstallStage("done");
        setInstallMessage("The engine is ready.");
        setInstallProgress(100);
        setError(null);
      } else {
        setInstallStage("error");
        setInstallError(
          value.error ??
            "The installer finished, but Nimbus could not find eddie-cli. Restart Nimbus and try again.",
        );
      }
    } catch (err) {
      if (mounted.current) {
        setInstallStage("error");
        setInstallError(String(err));
      }
    } finally {
      installInFlight.current = false;
      if (mounted.current) setInstalling(false);
    }
  }, []);

  const refreshEngine = useCallback(async () => {
    setInstallError(null);
    try {
      const value = await invoke<EngineInfo>("engine_info");
      if (!mounted.current) return;
      setInfo(value);
      setStatus(value.found ? "idle" : "absent");
      if (value.found) {
        setError(null);
        setInstallStage(null);
        setInstallMessage(null);
        setInstallProgress(null);
      } else if (value.error) {
        setError(value.error);
      }
    } catch (err) {
      if (mounted.current) setInstallError(String(err));
    }
  }, []);

  const retry = useCallback(() => {
    void start(optionsRef.current, false);
  }, [start]);

  const clearError = useCallback(() => setError(null), []);

  const releaseNetlock = useCallback(async () => {
    if (releasingLock) return "";
    setReleasingLock(true);
    try {
      const result = await invoke<{ ok: boolean; message: string }>("engine_disarm");
      if (result.ok) {
        setStatus("stopped");
        session.current = null;
      }
      return result.message;
    } catch (err) {
      return String(err);
    } finally {
      setReleasingLock(false);
    }
  }, [releasingLock]);

  const refreshLatency = useCallback(async () => {
    if (!enabled) return;
    setMeasuringLatency(true);
    try {
      const hosts = servers.map((s) => s.ip);
      const values = await invoke<Array<number | null>>("servers_latency", { hosts });
      const next: Record<string, number> = {};
      servers.forEach((s, i) => {
        const value = values[i];
        if (typeof value === "number") next[s.id] = value;
      });
      setLatencies(next);
    } catch {
      // Latency is cosmetic; keep whatever we had.
    } finally {
      setMeasuringLatency(false);
    }
  }, [enabled, servers]);

  useEffect(() => {
    if (enabled) void refreshLatency();
  }, [enabled, refreshLatency]);

  const latencyFor = useCallback(
    (s: ServerInfo): number | undefined => latencies[s.id],
    [latencies],
  );

  const selected = servers.find((s) => s.id === serverId) ?? servers[0];
  const actual = actualServerName
    ? servers.find((s) => s.name.toLowerCase() === actualServerName.toLowerCase())
    : undefined;
  const server = state === "connected" && actual ? actual : selected;
  const connected = state === "connected";

  useEffect(() => {
    if (connected) {
      setRecent((r) => [server.id, ...r.filter((x) => x !== server.id)].slice(0, 5));
    }
  }, [connected, server.id]);

  return {
    mode: "live" as const,
    state,
    server,
    serverId,
    servers,
    netlock,
    netlockAvailable,
    setNetlock,
    releaseNetlock,
    releasingLock,
    protocol,
    setProtocol,
    port,
    setPort,
    stats,
    favorites,
    recent,
    toggleFavorite: (id: string) =>
      setFavorites((f) => (f.includes(id) ? f.filter((x) => x !== id) : [...f, id])),
    connect,
    disconnect,
    stopForUpdate,
    toggle,
    selectServer,
    connected,
    // engine surface
    status,
    info,
    error,
    clearError,
    retry,
    ready: status === "ready",
    login,
    setLogin,
    password,
    setPassword,
    signIn,
    signingIn,
    signedIn,
    forgetSignIn,
    latencyFor,
    refreshLatency,
    measuringLatency,
    installEngine,
    installing,
    installStage,
    installMessage,
    installProgress,
    installError,
    refreshEngine,
  };
}

export type LiveEngine = ReturnType<typeof useLiveEngine>;
