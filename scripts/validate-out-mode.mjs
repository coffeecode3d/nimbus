#!/usr/bin/env node
/**
 * Nimbus `network.ipv4.mode=out` validation.
 *
 * App-based split tunneling needs Eddie to build the tunnel *without* claiming
 * the system default route: only traffic that Nimbus explicitly steers into the
 * tunnel should use it. Eddie calls that mode `out` (RouteAllIPv4 = false) and
 * pairs it with `routes.custom` entries whose action is `in`.
 *
 * This script proves or disproves that behaviour on a real host:
 *
 *   1. baseline: default route, rules, DNS
 *   2. sign in to a throwaway profile (Eddie's console client)
 *   3. connect with `--network.ipv4.mode=out` and two `in` routes
 *   4. check that the default route stayed on the physical interface, that the
 *      two listed destinations go through the tunnel, and that everything else
 *      does not
 *   5. disconnect and check that every tunnel route is gone again
 *
 *   node scripts/validate-out-mode.mjs --baseline
 *   AIRVPN_USER=... AIRVPN_PASS=... node scripts/validate-out-mode.mjs
 *   node scripts/validate-out-mode.mjs --creds ~/.config/nimbus/validate.env
 *
 * IMPORTANT
 *  - Quit Eddie (the GUI or Nimbus) first. Its single-instance lock and its
 *    privileged service belong to the running install; a second engine is
 *    refused.
 *  - The password is passed on eddie-cli's command line, exactly as Nimbus does
 *    it, because Eddie's jsoninout `options.set` ignores password-type options.
 *    It is therefore briefly visible in `ps` to other local users. Prefer the
 *    `--creds` file (chmod 600) or the environment over `--password` on the
 *    command line.
 *  - A throwaway profile is used, so Nimbus's saved session is never touched.
 *  - This script changes routing while it runs and restores it on disconnect.
 *    If it is killed mid-session, run it again or disconnect from Eddie.
 */

import { spawn, spawnSync } from "node:child_process";
import {
  appendFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  writeFileSync,
} from "node:fs";
import { createInterface } from "node:readline";
import { dirname, join } from "node:path";
import { tmpdir } from "node:os";

// The two destinations the tunnel is told to carry. 203.0.113.0/24 is TEST-NET-3
// (RFC 5737), so its presence in the routing table is the only observable —
// nothing will actually answer on it.
const IN_HOST = "1.1.1.1";
const IN_NET = "203.0.113.0/24";
const IN_NET_PROBE = "203.0.113.5";
// A well-known address that must keep using the physical interface.
const OUT_PROBE = "8.8.8.8";

const argv = process.argv.slice(2);
/** Accept both `--name=value` and `--name value`. */
const flagValue = (name) => {
  const eq = argv.find((a) => a.startsWith(`--${name}=`));
  if (eq) return eq.slice(name.length + 3);
  const i = argv.indexOf(`--${name}`);
  if (i !== -1 && i + 1 < argv.length && !argv[i + 1].startsWith("--")) {
    return argv[i + 1];
  }
  return undefined;
};
const baselineOnly = argv.includes("--baseline");
const keepProfile = argv.includes("--keep");
const timeoutSec = Number(flagValue("timeout") ?? process.env.VALIDATE_TIMEOUT ?? 90);
const ipv6Mode = flagValue("ipv6") ?? "out";
/**
 * Eddie's post-connect DNS check resolves a hash domain through the tunnel's
 * resolver and asks the exit whether it saw the query. In `out` mode there is
 * no default route through the tunnel, so that check cannot pass: the session
 * is reset and the engine loops on "Checking DNS failed". `dns.check` is a
 * documented Eddie option (Settings → DNS check), so the validator turns it off
 * by default. `--dns-check=on` keeps it, to reproduce the failure.
 */
const dnsCheck = (flagValue("dns-check") ?? "off").toLowerCase() !== "off";
/** "wireguard" (default), "openvpn", or "auto". */
const protocol = (flagValue("protocol") ?? "wireguard").toLowerCase();

function die(msg) {
  console.error(`\n  ✖ ${msg}\n`);
  process.exit(1);
}

function note(msg) {
  console.log(`  ${msg}`);
}

// ── credentials ────────────────────────────────────────────────────────────

/** Read `KEY=value` lines from a file without echoing anything. */
function readCredsFile(path) {
  const out = {};
  for (const raw of readFileSync(path, "utf8").split("\n")) {
    const line = raw.trim();
    if (!line || line.startsWith("#")) continue;
    const eq = line.indexOf("=");
    if (eq < 1) continue;
    const key = line.slice(0, eq).trim();
    let value = line.slice(eq + 1).trim();
    if (
      (value.startsWith('"') && value.endsWith('"')) ||
      (value.startsWith("'") && value.endsWith("'"))
    ) {
      value = value.slice(1, -1);
    }
    out[key] = value;
  }
  return out;
}

const credsFile = flagValue("creds");
let login = flagValue("login") ?? process.env.AIRVPN_USER ?? "";
let password = flagValue("password") ?? process.env.AIRVPN_PASS ?? "";
if (credsFile) {
  if (!existsSync(credsFile)) die(`Credentials file not found: ${credsFile}`);
  const creds = readCredsFile(credsFile);
  login = login || creds.AIRVPN_USER || creds.login || "";
  password = password || creds.AIRVPN_PASS || creds.password || "";
}

// ── engine discovery ───────────────────────────────────────────────────────

function findCli() {
  const candidates = [
    process.env.EDDIE_CLI,
    process.env.NIMBUS_EDDIE_CLI,
    "/opt/eddie-ui/eddie-cli",
    "/usr/lib/eddie-ui/eddie-cli",
    "/usr/local/lib/eddie-ui/eddie-cli",
  ].filter(Boolean);

  for (const c of candidates) if (existsSync(c)) return c;

  for (const entry of readdirSync("/tmp")) {
    if (!entry.startsWith(".mount_eddie")) continue;
    const p = `/tmp/${entry}/opt/eddie-ui/eddie-cli`;
    if (existsSync(p)) return p;
  }
  return null;
}

function runningEddie() {
  const out = [];
  for (const entry of readdirSync("/tmp")) {
    if (!/^Eddie_.*\.pid$/.test(entry)) continue;
    try {
      const pid = Number(readFileSync(join("/tmp", entry), "ascii").trim());
      if (pid > 0 && existsSync(`/proc/${pid}`)) out.push(pid);
    } catch {
      /* ignore */
    }
  }
  return out;
}

// ── routing helpers ────────────────────────────────────────────────────────

function sh(cmd, args) {
  const r = spawnSync(cmd, args, { encoding: "utf8" });
  if (r.error) die(`Could not run ${cmd}: ${r.error.message}`);
  return (r.stdout ?? "").trim();
}

const routes4 = () => sh("ip", ["-4", "route", "show"]);
const routes6 = () => sh("ip", ["-6", "route", "show"]);
const rules = () => sh("ip", ["rule", "show"]);
const links = () => sh("ip", ["-br", "link"]);

/** Interface names currently known to the kernel. `ip -br link` is
 *  `<name> <state> <mac>`, so the name is field 0. */
function linkNames() {
  return new Set(
    links()
      .split("\n")
      .map((l) => l.trim().split(/\s+/)[0])
      .filter(Boolean),
  );
}

/** Best-effort device kind, for the report only. */
function linkKind(dev) {
  const out = sh("ip", ["-d", "link", "show", dev]);
  if (/wireguard/i.test(out)) return "wireguard";
  if (/\btun\b/i.test(out)) return "tun";
  return "";
}

function defaultDev() {
  const line = routes4()
    .split("\n")
    .find((l) => l.startsWith("default "));
  if (!line) return null;
  return /dev (\S+)/.exec(line)?.[1] ?? null;
}

function defaultLine6() {
  return routes6()
    .split("\n")
    .find((l) => l.startsWith("default ")) ?? null;
}

/** `ip route get <ip>` → { dev, via, raw } */
function routeGet(ip) {
  const raw = sh("ip", ["-4", "route", "get", ip]).split("\n")[0] ?? "";
  return {
    raw,
    dev: /dev (\S+)/.exec(raw)?.[1] ?? null,
    via: /via (\S+)/.exec(raw)?.[1] ?? null,
  };
}

function dnsServers() {
  const out = [];
  try {
    for (const line of readFileSync("/etc/resolv.conf", "utf8").split("\n")) {
      const m = /^nameserver\s+(\S+)/.exec(line.trim());
      if (m) out.push(m[1]);
    }
  } catch {
    /* no resolv.conf */
  }
  return out;
}

function tunnelDevices(baselineLinks) {
  const now = linkNames();
  return [...now].filter((n) => !baselineLinks.has(n));
}

function snapshot() {
  return {
    defaultDev: defaultDev(),
    default6: defaultLine6(),
    routes4: routes4(),
    routes6: routes6(),
    rules: rules(),
    dns: dnsServers(),
  };
}

function printSnapshot(label, snap) {
  console.log(`\n  ── ${label} ──`);
  note(`default dev     ${snap.defaultDev ?? "(none)"}`);
  note(`default v6      ${snap.default6 ?? "(none)"}`);
  note(`dns             ${snap.dns.join(", ") || "(none)"}`);
  console.log("  routes:");
  for (const line of snap.routes4.split("\n").filter(Boolean)) {
    console.log(`    ${line}`);
  }
  if (snap.rules) {
    console.log("  rules:");
    for (const line of snap.rules.split("\n").filter(Boolean)) {
      console.log(`    ${line}`);
    }
  }
}

// ── preflight ──────────────────────────────────────────────────────────────

const cli = findCli();
if (!cli) {
  die(
    "Could not find eddie-cli. Set EDDIE_CLI=/path/to/eddie-cli, or install the\n" +
      "  official Eddie package so it lives at /opt/eddie-ui/eddie-cli.",
  );
}
if (process.platform !== "linux") {
  die("This validator is Linux-only. Windows needs a different route probe.");
}

const running = runningEddie();
if (running.length > 0) {
  die(
    `Eddie is running (pid ${running.join(", ")}).\n` +
      "  Quit Nimbus/Eddie first — the privileged service and the single-instance\n" +
      "  lock belong to that install.",
  );
}

const baselineLinks = linkNames();
// Eddie names its WireGuard device "Eddie", OpenVPN gets a tun device. Catch any
// leftover so the baseline means something.
const preexisting = [...baselineLinks].filter((n) => /^(wg|tun|tap)\d*$/.test(n) || n === "Eddie");
if (preexisting.length > 0) {
  die(
    `A tunnel interface is already up (${preexisting.join(", ")}).\n` +
      "  Disconnect it first so the baseline is meaningful.",
  );
}

const dataDir = join(tmpdir(), "nimbus-outmode");
const profileDir = join(dataDir, "profile");
const tmpDir = join(profileDir, "tmp");
const logPath = join(dataDir, "engine.log");
mkdirSync(tmpDir, { recursive: true });
writeFileSync(logPath, "");

console.log(`
  Nimbus "mode=out" validation
  ────────────────────────────
  binary   ${cli}
  profile  ${profileDir}
  log      ${logPath}
  ipv4     out
  ipv6     ${ipv6Mode}
  proto    ${protocol}
  dns      ${dnsCheck ? "check enabled (will fail in out mode)" : "check disabled, resolver untouched"}
  routes   ${IN_HOST},in ; ${IN_NET},in

  A polkit prompt may appear — eddie-cli is raising its privileged helper.
`);

const before = snapshot();
printSnapshot("baseline", before);

if (baselineOnly) {
  console.log(`
  Baseline captured. Re-run without --baseline to test a real connection.
`);
  process.exit(0);
}

if (!login || !password) {
  die(
    "Missing credentials.\n\n" +
      "  Provide them in one of these ways:\n" +
      "    AIRVPN_USER=you AIRVPN_PASS=... node scripts/validate-out-mode.mjs\n" +
      "    node scripts/validate-out-mode.mjs --creds /path/to/creds.env\n" +
      "    node scripts/validate-out-mode.mjs --login=you --password=...\n\n" +
      "  The creds file is two lines:\n" +
      "    AIRVPN_USER=your-username\n" +
      "    AIRVPN_PASS=your-password\n",
  );
}

// ── engine runs ────────────────────────────────────────────────────────────

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const NETLOCK_OFF = [
  "--netlock.mode=none",
  "--netlock=false",
  "--netlock.connection=false",
];

const baseEnv = { ...process.env, TMPDIR: tmpDir };

/**
 * Eddie's jsoninout engine cannot sign in (there is no login command), so the
 * profile has to be authorized once by the console client first. Mirrors
 * `authenticate_inner` in src-tauri/src/engine.rs.
 */
async function signIn() {
  console.log("  signing in to the throwaway profile…");
  const args = [
    `--path=${profileDir}`,
    "--connect=false",
    ...NETLOCK_OFF,
    "--remember=true",
    "--advanced.expert=true",
    `--login=${login.trim()}`,
    `--password=${password}`,
  ];
  const child = spawn(cli, args, {
    cwd: dirname(cli),
    env: baseEnv,
    stdio: ["ignore", "pipe", "pipe"],
  });

  let ok = false;
  let reason = "Eddie did not finish signing in.";
  const rl = createInterface({ input: child.stdout });
  rl.on("line", (line) => {
    appendFileSync(logPath, `[login] ${line}\n`);
    if (line.includes("Logged in.")) ok = true;
    if (line.includes("Wrong login/password")) reason = "Wrong AirVPN username or password.";
    if (line.includes("Cannot login")) reason = line.split(" - ").pop()?.trim() ?? reason;
  });
  child.stderr.on("data", (d) => appendFileSync(logPath, `[login stderr] ${String(d)}`));

  const deadline = Date.now() + 45_000;
  while (Date.now() < deadline && !ok && !reason.includes("Wrong")) {
    await sleep(250);
    if (child.exitCode !== null) break;
  }
  child.kill("SIGTERM");
  await sleep(500);
  if (child.exitCode === null) child.kill("SIGKILL");
  if (!ok) die(reason);
  note("signed in");
}

/** Start a connecting engine and resolve once it reports a live session. */
function connect() {
  console.log("  starting the connecting engine…");
  const args = [
    "--jsoninout",
    `--path=${profileDir}`,
    "--connect=false",
    "--remember=true",
    ...NETLOCK_OFF,
    "--network.ipv4.mode=out",
    `--network.ipv6.mode=${ipv6Mode}`,
    `--mode.type=${protocol}`,
    `--dns.check=${dnsCheck ? "true" : "false"}`,
    // Leave the host resolver alone. Include mode supplies its own
    // tunnel-bound resolver, and a validation run must not rewrite the
    // machine's DNS (Eddie otherwise restarts systemd-resolved).
    "--dns.mode=none",
    `--routes.custom=${IN_HOST},in,validation-host;${IN_NET},in,validation-net`,
    `--login=${login.trim()}`,
    `--password=${password}`,
  ];
  const child = spawn(cli, args, {
    cwd: dirname(cli),
    env: baseEnv,
    stdio: ["pipe", "pipe", "pipe"],
  });

  const state = { connected: false, failed: null, ready: false, errors: [] };
  const rl = createInterface({ input: child.stdout });
  rl.on("line", (line) => {
    appendFileSync(logPath, `[engine] ${line}\n`);
    if (!line.trim().startsWith("{")) return;
    let obj;
    try {
      obj = JSON.parse(line);
    } catch {
      return;
    }
    if (obj.command === "engine.ready") state.ready = true;
    if (obj.action_icon === "connected") state.connected = true;
    if (typeof obj.error === "string" && obj.error) state.errors.push(obj.error);
    if (typeof obj.message === "string" && /error|failed|wrong/i.test(obj.message)) {
      state.errors.push(obj.message);
    }
  });
  child.stderr.on("data", (d) => appendFileSync(logPath, `[engine stderr] ${String(d)}`));

  const send = (obj) => {
    if (child.stdin.writable) child.stdin.write(JSON.stringify(obj) + "\n");
  };

  return {
    child,
    state,
    send,
    stop: async () => {
      send({ command: "mainaction.disconnect" });
      const deadline = Date.now() + 45_000;
      while (Date.now() < deadline) {
        await sleep(500);
        if (tunnelDevices(baselineLinks).length === 0) break;
      }
      send({ command: "exit" });
      const stop = Date.now() + 5_000;
      while (Date.now() < stop && child.exitCode === null) await sleep(100);
      if (child.exitCode === null) child.kill("SIGTERM");
      await sleep(500);
      if (child.exitCode === null) child.kill("SIGKILL");
    },
  };
}

/** Eddie populates its server list *after* engine.ready; a connect sent too
 *  early is silently dropped by CanConnect(). Retry like the app does. */
async function waitForConnect(session) {
  const deadline = Date.now() + timeoutSec * 1000;
  let attempts = 0;
  let lastAttempt = 0;
  while (Date.now() < deadline) {
    if (session.state.connected) return true;
    if (session.child.exitCode !== null) return false;
    if (session.state.ready && Date.now() - lastAttempt > 2000 && attempts < 8) {
      attempts += 1;
      lastAttempt = Date.now();
      session.send({ command: "mainaction.connect" });
    }
    await sleep(500);
  }
  return false;
}

// ── assertions ─────────────────────────────────────────────────────────────

const results = [];
function check(name, ok, detail) {
  results.push({ name, ok, detail });
  console.log(`  ${ok ? "PASS" : "FAIL"}  ${name}${detail ? `  — ${detail}` : ""}`);
}

async function main() {
  await signIn();
  const session = connect();

  let connected = false;
  let tunnelDev = null;
  try {
    connected = await waitForConnect(session);
    if (!connected) {
      console.log("\n  ── engine log tail ──");
      const tail = readFileSync(logPath, "utf8").split("\n").slice(-25);
      for (const line of tail) console.log(`    ${line}`);
      if (session.state.errors.length) {
        console.log(`  errors: ${[...new Set(session.state.errors)].slice(0, 5).join(" | ")}`);
      }
      die(`No session within ${timeoutSec}s. See ${logPath}.`);
    }

    // Eddie installs routes right after the interface comes up.
    await sleep(2500);
    const during = snapshot();
    printSnapshot("connected", during);

    const phys = before.defaultDev;
    // Never assume the device name. Eddie calls its WireGuard device "Eddie";
    // OpenVPN gets a tun device. Read the name back from the route that the
    // custom `in` entry produced, and fall back to the new-link diff.
    const inHostRoute = routeGet(IN_HOST);
    tunnelDev =
      inHostRoute.dev && inHostRoute.dev !== phys
        ? inHostRoute.dev
        : (tunnelDevices(baselineLinks)[0] ?? null);
    const tunnel = tunnelDev;

    if (!tunnel) {
      check(
        "tunnel device identified",
        false,
        `route for ${IN_HOST} used ${inHostRoute.dev ?? "no device"}`,
      );
    } else {
      note(`tunnel device   ${tunnel} (${linkKind(tunnel) || "unknown kind"})`);
      check(
        "tunnel device is not the physical interface",
        tunnel !== phys,
        `${tunnel} vs ${phys}`,
      );
    }

    const dflt = during.defaultDev;
    check(
      "default route stayed on the physical interface",
      dflt === phys,
      `baseline ${phys ?? "(none)"} → now ${dflt ?? "(none)"}`,
    );

    if (tunnel) {
      const viaTunnel = during.routes4
        .split("\n")
        .filter((l) => l.startsWith("default ") && l.includes(`dev ${tunnel}`));
      check("no IPv4 default route through the tunnel", viaTunnel.length === 0, viaTunnel.join(" | "));
    }

    const host = inHostRoute;
    check(
      `listed host ${IN_HOST} uses the tunnel`,
      tunnel !== null && host.dev === tunnel,
      host.raw || "no route",
    );

    const net = routeGet(IN_NET_PROBE);
    check(
      `listed subnet ${IN_NET} uses the tunnel`,
      tunnel !== null && net.dev === tunnel,
      net.raw || "no route",
    );

    const other = routeGet(OUT_PROBE);
    check(
      `unlisted ${OUT_PROBE} stays on the physical interface`,
      other.dev === phys,
      other.raw || "no route",
    );

    if (during.default6) {
      check(
        "no IPv6 default route through the tunnel",
        tunnel === null || !during.default6.includes(`dev ${tunnel}`),
        during.default6,
      );
    } else {
      note("no IPv6 default route at all (nothing to check)");
    }

    note(`dns while connected: ${during.dns.join(", ") || "(none)"}`);
    note(`rules while connected: ${during.rules.replace(/\n/g, " ; ")}`);
  } finally {
    console.log("\n  disconnecting…");
    await session.stop();
    await sleep(2000);
    const after = snapshot();
    printSnapshot("after disconnect", after);

    check(
      "default route restored",
      after.defaultDev === before.defaultDev,
      `${before.defaultDev ?? "(none)"} → ${after.defaultDev ?? "(none)"}`,
    );

    const leftover = [...linkNames()].filter(
      (n) => /^(wg|tun|tap)\d*$/.test(n) || n === tunnelDev,
    );
    check("tunnel interface removed", leftover.length === 0, leftover.join(", "));

    const stale = after.routes4
      .split("\n")
      .filter((l) => l.includes(IN_HOST) || l.includes(IN_NET));
    check("custom routes removed", stale.length === 0, stale.join(" | "));
  }

  const failed = results.filter((r) => !r.ok);
  const reportPath = join(dataDir, "report.json");
  writeFileSync(
    reportPath,
    JSON.stringify({ at: new Date().toISOString(), before, results }, null, 2),
  );

  console.log(`
  ────────────────────────────
  ${results.length - failed.length}/${results.length} checks passed
  log     ${logPath}
  report  ${reportPath}
`);
  if (!keepProfile) note("the throwaway profile keeps the signed-in session for a rerun");
  process.exit(failed.length === 0 ? 0 : 1);
}

main().catch((err) => die(err?.stack ?? String(err)));
