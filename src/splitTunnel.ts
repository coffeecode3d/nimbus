/**
 * Destination-based split tunneling for Nimbus.
 *
 * Eddie already understands a `routes.custom` profile option in the form
 * `{ip or range},{in/out},{note};...`. An `out` entry routes the destination
 * through the physical gateway, so it is excluded from the tunnel. Nimbus only
 * builds and validates that string; Eddie's privileged helper still installs
 * every route. Phase 1 is IPv4-only and exclude-only.
 */

export const SPLIT_TUNNEL_KEY = "nimbus.splitTunnel";
export const MAX_SPLIT_RULES = 64;

export type SplitTunnelPreset = "lan" | "multicast";

export interface SplitTunnelRule {
  id: string;
  /** Normalized IPv4 network, e.g. `192.168.0.0/16`. */
  cidr: string;
  /** Optional user note, passed to Eddie as the route note. */
  label: string;
  preset?: SplitTunnelPreset;
}

export interface SplitTunnelConfig {
  enabled: boolean;
  rules: SplitTunnelRule[];
}

export interface ParsedCidr {
  cidr: string;
  network: number;
  prefix: number;
  size: number;
  /** True when host bits were cleared, e.g. 192.168.1.7/24. */
  normalized: boolean;
}

export type CidrParseResult =
  | { ok: true; value: ParsedCidr }
  | { ok: false; error: string };

export interface SplitTunnelPresetDef {
  id: SplitTunnelPreset;
  label: string;
  description: string;
  cidrs: string[];
}

export const SPLIT_TUNNEL_PRESETS: Record<SplitTunnelPreset, SplitTunnelPresetDef> = {
  lan: {
    id: "lan",
    label: "Local network",
    description: "Private LAN and link-local ranges",
    cidrs: ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "169.254.0.0/16"],
  },
  multicast: {
    id: "multicast",
    label: "Multicast",
    description: "Local service discovery and multicast groups",
    cidrs: ["224.0.0.0/4"],
  },
};

const OCTET = /^\d{1,3}$/;
const PREFIX = /^\d{1,2}$/;

/** Strict dotted-quad parser. Rejects leading zeros so `010` is not octal. */
function ipv4ToInt(text: string): number | null {
  const parts = text.split(".");
  if (parts.length !== 4) return null;
  let value = 0;
  for (const part of parts) {
    if (!OCTET.test(part)) return null;
    if (part.length > 1 && part.startsWith("0")) return null;
    const octet = Number(part);
    if (octet > 255) return null;
    value = value * 256 + octet;
  }
  return value >>> 0;
}

function intToIpv4(value: number): string {
  const n = value >>> 0;
  return [(n >>> 24) & 255, (n >>> 16) & 255, (n >>> 8) & 255, n & 255].join(".");
}

function maskForPrefix(prefix: number): number {
  return prefix === 0 ? 0 : (0xffffffff << (32 - prefix)) >>> 0;
}

/** Parse and normalize an IPv4 address or CIDR. IPv6 is rejected in Phase 1. */
export function parseCidr(input: string): CidrParseResult {
  const raw = input.trim();
  if (!raw) return { ok: false, error: "Enter an IPv4 address or subnet." };
  if (raw.includes(":")) {
    return {
      ok: false,
      error: "IPv6 is not supported yet. Use an IPv4 address or subnet.",
    };
  }

  const slash = raw.indexOf("/");
  if (slash !== -1 && raw.indexOf("/", slash + 1) !== -1) {
    return { ok: false, error: "Use a single /prefix, for example 192.168.0.0/16." };
  }

  const addressPart = (slash === -1 ? raw : raw.slice(0, slash)).trim();
  const prefixPart = (slash === -1 ? "32" : raw.slice(slash + 1)).trim();

  const address = ipv4ToInt(addressPart);
  if (address === null) {
    return { ok: false, error: `“${addressPart}” is not a valid IPv4 address.` };
  }

  if (!PREFIX.test(prefixPart)) {
    return { ok: false, error: "The prefix must be a number from 0 to 32." };
  }
  const prefix = Number(prefixPart);
  if (prefix > 32) {
    return { ok: false, error: "The prefix must be a number from 0 to 32." };
  }
  if (prefix === 0) {
    return {
      ok: false,
      error:
        "Excluding 0.0.0.0/0 would send everything outside the VPN. Add narrower ranges instead.",
    };
  }

  const mask = maskForPrefix(prefix);
  const network = (address & mask) >>> 0;
  const cidr = `${intToIpv4(network)}/${prefix}`;
  return {
    ok: true,
    value: {
      cidr,
      network,
      prefix,
      size: 2 ** (32 - prefix),
      normalized: network !== address,
    },
  };
}

/** Return the first existing rule whose range overlaps `value`. */
export function findOverlap(
  rules: SplitTunnelRule[],
  value: ParsedCidr,
): SplitTunnelRule | null {
  const valueEnd = value.network + value.size - 1;
  for (const rule of rules) {
    const parsed = parseCidr(rule.cidr);
    if (!parsed.ok) continue;
    const other = parsed.value;
    const otherEnd = other.network + other.size - 1;
    if (value.network <= otherEnd && other.network <= valueEnd) return rule;
  }
  return null;
}

/** Strip the separators Eddie uses so a label cannot break the route string. */
export function sanitizeLabel(label: string): string {
  return label
    .replace(/[;,\r\n\t]/g, " ")
    .replace(/[\u0000-\u001f\u007f]/g, "")
    .replace(/\s+/g, " ")
    .trim()
    .slice(0, 48);
}

/** Build Eddie's `routes.custom` value. Disabled means an empty override. */
export function serializeSplitTunnel(config: SplitTunnelConfig): string {
  if (!config.enabled) return "";
  return config.rules
    .map((rule) => {
      const note = sanitizeLabel(rule.label);
      return note ? `${rule.cidr},out,${note}` : `${rule.cidr},out`;
    })
    .join(";");
}

let fallbackId = 0;

export function newRuleId(): string {
  const uuid = globalThis.crypto?.randomUUID?.();
  if (uuid) return uuid;
  fallbackId += 1;
  return `st${Date.now().toString(36)}${fallbackId.toString(36)}`;
}

export function presetLabel(preset: SplitTunnelPreset | undefined): string | null {
  return preset ? SPLIT_TUNNEL_PRESETS[preset].label : null;
}

/** Rules for a preset, normalized and ready to append. */
export function presetRules(preset: SplitTunnelPreset): SplitTunnelRule[] {
  return SPLIT_TUNNEL_PRESETS[preset].cidrs.flatMap((cidr) => {
    const parsed = parseCidr(cidr);
    if (!parsed.ok) return [];
    return [{ id: newRuleId(), cidr: parsed.value.cidr, label: "", preset }];
  });
}

function isPreset(value: unknown): value is SplitTunnelPreset {
  return value === "lan" || value === "multicast";
}

function normalizeRule(value: unknown): SplitTunnelRule | null {
  if (!value || typeof value !== "object") return null;
  const record = value as Record<string, unknown>;
  if (typeof record.cidr !== "string") return null;
  const parsed = parseCidr(record.cidr);
  if (!parsed.ok) return null;
  return {
    id: typeof record.id === "string" && record.id ? record.id : newRuleId(),
    cidr: parsed.value.cidr,
    label: typeof record.label === "string" ? sanitizeLabel(record.label) : "",
    preset: isPreset(record.preset) ? record.preset : undefined,
  };
}

/** Read the saved configuration, dropping anything that no longer validates. */
export function loadSplitTunnel(): SplitTunnelConfig {
  try {
    const raw = localStorage.getItem(SPLIT_TUNNEL_KEY);
    if (!raw) return { enabled: false, rules: [] };
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") return { enabled: false, rules: [] };
    const record = parsed as Record<string, unknown>;
    const rules: SplitTunnelRule[] = [];
    if (Array.isArray(record.rules)) {
      for (const item of record.rules) {
        const rule = normalizeRule(item);
        if (!rule) continue;
        if (rules.some((existing) => existing.cidr === rule.cidr)) continue;
        if (rules.length >= MAX_SPLIT_RULES) break;
        rules.push(rule);
      }
    }
    return { enabled: record.enabled === true, rules };
  } catch {
    return { enabled: false, rules: [] };
  }
}

export function saveSplitTunnel(config: SplitTunnelConfig): void {
  try {
    localStorage.setItem(SPLIT_TUNNEL_KEY, JSON.stringify(config));
  } catch {
    // Preferences are best-effort when localStorage is unavailable.
  }
}
