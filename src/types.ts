export type ConnState =
  | "disconnected"
  | "connecting"
  | "connected"
  | "disconnecting"
  | "blocked";

export type Protocol = "wireguard" | "openvpn";

export interface ServerInfo {
  id: string;
  name: string;
  city: string;
  country: string;
  cc: string;
  continent: string;
  load: number;
  users: number;
  ip: string;
  bw: number;
}

export interface Stats {
  down: number;
  up: number;
  seconds: number;
  samples: number[];
  upSamples: number[];
}

export type View = "connect" | "servers" | "settings";

export interface EngineInfo {
  found: boolean;
  cli: string;
  appDir: string;
  profile: string;
  error: string | null;
}

export type EngineStatus = "absent" | "idle" | "starting" | "ready" | "stopped" | "error";

/** The surface both the live engine adapter and the preview simulator expose. */
export interface EngineApi {
  mode: "live" | "mock";
  state: ConnState;
  server: ServerInfo;
  serverId: string;
  servers: ServerInfo[];
  netlock: boolean;
  /** False while the live engine build keeps the lock off for safety. */
  netlockAvailable: boolean;
  setNetlock: (value: boolean) => void;
  /** Emergency firewall release for a lock left behind by a failed session. */
  releaseNetlock: () => Promise<string>;
  releasingLock: boolean;
  protocol: Protocol;
  setProtocol: (value: Protocol) => void;
  port: number;
  setPort: (value: number) => void;
  stats: Stats;
  favorites: string[];
  recent: string[];
  toggleFavorite: (id: string) => void;
  connect: () => void;
  disconnect: () => void;
  toggle: () => void;
  selectServer: (id: string) => void;
  connected: boolean;
  status: EngineStatus;
  info: EngineInfo | null;
  error: string | null;
  clearError: () => void;
  retry: () => void;
  ready: boolean;
  login: string;
  setLogin: (value: string) => void;
  password: string;
  setPassword: (value: string) => void;
  signIn: () => Promise<void>;
  signingIn: boolean;
  signedIn: boolean;
  /** Remove the saved password from the OS keyring, keeping the account name. */
  forgetSignIn: () => Promise<void>;
  latencyFor: (server: ServerInfo) => number | undefined;
  refreshLatency: () => Promise<void>;
  measuringLatency: boolean;
}
