#!/usr/bin/env node
/**
 * Nimbus engine probe.
 *
 * Spawns the official eddie-cli in --jsoninout mode, logs every JSON event,
 * and (optionally) asks it a set of read-only commands to learn what the
 * installed build actually supports.
 *
 *   node scripts/probe.mjs                 # watch the event stream
 *   node scripts/probe.mjs --discover      # also probe read-only commands
 *   node scripts/probe.mjs --cmd '{"command":"mainaction.disconnect"}'
 *
 * IMPORTANT
 *  - Quit Eddie first. Its single-instance lock and its privileged service
 *    are bound to the running install; a second engine is refused.
 *  - The first run triggers a polkit prompt, because eddie-cli raises its own
 *    privileged helper. Nimbus never does this itself.
 *  - This script does not change firewall or DNS settings: netlock is disabled
 *    and connect defaults to false.
 */

import { spawn } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync, appendFileSync } from "node:fs";
import { createInterface } from "node:readline";
import { dirname, join } from "node:path";
import { tmpdir } from "node:os";

const argv = process.argv.slice(2);
const discover = argv.includes("--discover");
const cmdArgs = argv.flatMap((a, i) => (a === "--cmd" ? [argv[i + 1]] : [])).filter(Boolean);

function die(msg) {
  console.error(`\n  ✖ ${msg}\n`);
  process.exit(1);
}

function findCli() {
  const explicit = process.env.EDDIE_CLI;
  const candidates = [
    explicit,
    "/opt/eddie-ui/eddie-cli",
    "/usr/lib/eddie-ui/eddie-cli",
    "/usr/local/lib/eddie-ui/eddie-cli",
  ].filter(Boolean);

  for (const c of candidates) if (existsSync(c)) return c;

  // Running AppImage mounts (read-only squashfs).
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
      if (pid > 0 && existsSync(`/proc/${pid}`)) out.push({ pid, file: entry });
    } catch {
      /* ignore */
    }
  }
  return out;
}

const cli = findCli();
if (!cli) {
  die(
    "Could not find eddie-cli. Set EDDIE_CLI=/path/to/eddie-cli, or install the official\n" +
      "  Eddie package (deb/rpm/AUR) so it lives at /opt/eddie-ui/eddie-cli.",
  );
}

const running = runningEddie();
if (running.length > 0) {
  die(
    `Eddie is running (pid ${running.map((r) => r.pid).join(", ")}).\n` +
      "  Quit it from the tray first — the privileged service and the single-instance\n" +
      "  lock belong to that install.",
  );
}

const dataDir = join(tmpdir(), "nimbus-probe");
const tmpDir = join(dataDir, "tmp");
mkdirSync(tmpDir, { recursive: true });
const logPath = join(dataDir, "probe.log");
writeFileSync(logPath, "");

console.log(`
  Nimbus engine probe
  ───────────────────
  binary   ${cli}
  profile  ${dataDir}
  log      ${logPath}
  mode     ${discover ? "discover" : "listen"}${cmdArgs.length ? ` + ${cmdArgs.length} command(s)` : ""}

  A polkit prompt may appear — eddie-cli is raising its privileged helper.
  Nothing will connect. Quit with Ctrl+C.
`);

const child = spawn(
  cli,
  ["--jsoninout", `--path=${dataDir}`, "--netlock.mode=none", "--connect=false"],
  {
    cwd: dirname(cli), // the privileged helper requires the client to live beside it
    env: { ...process.env, TMPDIR: tmpDir },
    stdio: ["pipe", "pipe", "pipe"],
  },
);

const seen = new Map();
let lines = 0;
let ready = false;

function record(obj) {
  const cmd = obj.command ?? "(no command)";
  const counts = seen.get(cmd) ?? 0;
  seen.set(cmd, counts + 1);
  if (counts > 6) return; // keep the console readable; the log keeps everything

  const skip = new Set(["id", "command", "time"]);
  const detail = Object.entries(obj)
    .filter(([k, v]) => !skip.has(k) && v !== "" && v != null)
    .map(([k, v]) => `${k}=${typeof v === "object" ? JSON.stringify(v) : v}`)
    .join("  ");

  console.log(`  ${cmd.padEnd(22)} ${detail.slice(0, 160)}`);
}

const rl = createInterface({ input: child.stdout });
rl.on("line", (line) => {
  appendFileSync(logPath, line + "\n");
  if (!line.trim().startsWith("{")) {
    console.log(`  raw: ${line}`);
    return;
  }
  lines += 1;
  let obj;
  try {
    obj = JSON.parse(line);
  } catch {
    return;
  }
  record(obj);

  if (obj.command === "engine.ready" && !ready) {
    ready = true;
    if (discover || cmdArgs.length) runCommands();
  }
});

child.stderr.on("data", (d) => {
  const text = String(d).trim();
  if (text) appendFileSync(logPath, `[stderr] ${text}\n`);
});

child.on("exit", (code, signal) => {
  console.log(`
  ── engine exited (code=${code ?? "null"} signal=${signal ?? "none"}) ──
  events: ${lines}   log: ${logPath}
  commands seen:`);
  for (const [cmd, n] of [...seen.entries()].sort((a, b) => b[1] - a[1])) {
    console.log(`    ${String(n).padStart(4)}  ${cmd}`);
  }
  console.log();
  process.exit(0);
});

function send(obj) {
  child.stdin.write(JSON.stringify(obj) + "\n");
}

const DISCOVERY = [
  { command: "options.get", name: "server" },
  { command: "options.get", name: "protocol" },
  { command: "servers.list" },
  { command: "connection.info" },
  { command: "connection.status" },
  { command: "session.info" },
  { command: "stats.get" },
  { command: "ui.manifest" },
  { command: "engine.version" },
];

async function runCommands() {
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  await sleep(600);

  const list = [...(discover ? DISCOVERY : []), ...cmdArgs.map((c) => JSON.parse(c))];
  for (const cmd of list) {
    console.log(`  → ${JSON.stringify(cmd)}`);
    send(cmd);
    await sleep(800);
  }
}

const timeout = Number(process.env.PROBE_TIMEOUT ?? 40) * 1000;
const killer = setTimeout(() => {
  console.log("\n  ── timeout, asking the engine to stop ──");
  send({ command: "engine.stdin.stop" });
  setTimeout(() => child.kill("SIGTERM"), 2000);
}, timeout);

process.on("SIGINT", () => {
  clearTimeout(killer);
  send({ command: "engine.stdin.stop" });
  child.stdin.end();
});
