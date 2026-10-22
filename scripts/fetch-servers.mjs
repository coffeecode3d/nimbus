#!/usr/bin/env node
/**
 * Refresh src/servers.json from AirVPN's public status API.
 *
 *   node scripts/fetch-servers.mjs
 *
 * The API needs no key. It is the same catalogue Eddie's own provider fetches,
 * reduced to the fields the UI renders.
 */
import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const target = join(here, "..", "src", "servers.json");

const response = await fetch("https://airvpn.org/api/status/", {
  headers: { "user-agent": "nimbus/0.1 (+https://github.com/; unofficial AirVPN client)" },
});
if (!response.ok) {
  console.error(`AirVPN API returned ${response.status}`);
  process.exit(1);
}
const payload = await response.json();
const rows = payload.servers;
if (!Array.isArray(rows) || rows.length === 0) {
  console.error("No servers in the API response.");
  process.exit(1);
}

const servers = rows
  .map((row) => ({
    id: String(row.public_name ?? "").toLowerCase(),
    name: row.public_name,
    city: row.location,
    country: row.country_name,
    cc: String(row.country_code ?? "").toUpperCase(),
    continent: row.continent,
    load: Math.max(0, Math.min(100, Number(row.currentload) || 0)),
    users: Number(row.users) || 0,
    ip: row.ip_v4_in1,
    bw: Number(row.bw_max) || Number(row.bw) || 0,
  }))
  .filter((s) => s.id && s.ip && s.name)
  .sort(
    (a, b) =>
      a.continent.localeCompare(b.continent) ||
      a.country.localeCompare(b.country) ||
      a.city.localeCompare(b.city) ||
      a.name.localeCompare(b.name),
  );

writeFileSync(target, JSON.stringify(servers, null, 2) + "\n");
console.log(`Wrote ${servers.length} servers to ${target}`);
