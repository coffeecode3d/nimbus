//! Pure planning for app-based split tunneling.
//!
//! Nothing in this module touches the system: it turns a description (tunnel
//! device, selected apps) into the exact rule and `ip` argument vectors that
//! [`crate::session`] applies. That keeps the interesting policy unit-testable
//! without root.
//!
//! Shape of the policy (include mode, Linux):
//!
//! * every socket owned by a process in the `nimbus.apps` cgroup is marked with
//!   [`MARK`] on output;
//! * `ip rule fwmark MARK lookup TABLE` sends marked packets to [`TABLE`];
//! * [`TABLE`] contains a default route through the tunnel, so only the
//!   selected apps are inside the VPN;
//! * IPv6 from those same sockets is dropped, because the prototype does not
//!   steer IPv6 and must not leak it outside the tunnel.
//!
//! # Why iptables rather than nftables
//!
//! For locally generated traffic the IPv4 route lookup happens *before* the
//! netfilter OUTPUT hook. Setting `meta mark` with nftables there attaches the
//! mark but does not reroute the packet: measured on kernel 7.3 with nft 1.1.7,
//! three marked datagrams still left the physical interface.
//!
//! The xtables `MARK` target has the behaviour we need: `iptable_mangle` calls
//! `ip_route_me_harder()` when a rule changes `skb->mark`, so the packet is
//! rerouted through table [`TABLE`] before it is transmitted. The same
//! measurement then shows all three datagrams leaving through the tunnel
//! device. This holds for both the legacy and the nf_tables-backed `iptables`.
//!
//! # Ordering requirement
//!
//! `-m cgroup --path` is resolved against the cgroup filesystem when the rule is
//! installed, so [`CGROUP_PATH`] must exist first. [`crate::session::Session::begin`]
//! therefore creates the cgroup before installing any rule.

/// Name of the cgroup directory directly below the cgroup v2 mount point.
/// `-m cgroup --path` takes it without a leading slash.
pub const CGROUP_NAME: &str = "nimbus.apps";

/// Absolute path of the cgroup the selected processes are moved into.
pub const CGROUP_PATH: &str = "/sys/fs/cgroup/nimbus.apps";

/// Root of the cgroup v2 hierarchy, used to move processes back out again.
pub const CGROUP_ROOT_PROCS: &str = "/sys/fs/cgroup/cgroup.procs";

/// Firewall mark applied to the selected apps' packets.
pub const MARK: u32 = 0x4e4d;

/// Routing table that carries the tunnel default route.
pub const TABLE: u32 = 100;

/// Priority of the policy rule. Above the default `main` rule (32766) so it is
/// consulted first, and below the `local` rule at 0 so loopback still wins.
pub const RULE_PREF: u32 = 1000;

/// Source-NAT marked packets to the address of the device they leave through.
///
/// Without this, an included app keeps the source address the kernel chose
/// *before* the mark existed (its LAN address), and the VPN exit drops the
/// packet. Measured: a marked probe left the tunnel device as
/// `SRC=192.168.6.124`, and conntrack expected the reply at the LAN address
/// instead of the tunnel one.
///
/// No `-o` device is given on purpose: marked packets only ever leave through
/// the tunnel (table [`TABLE`] has no other default), and leaving it out lets
/// teardown remove the rule without knowing the tunnel name.
pub fn masquerade_rule() -> Vec<String> {
    nat_rule(Op::Add)
}

pub fn masquerade_rule_check() -> Vec<String> {
    nat_rule(Op::Check)
}

pub fn masquerade_rule_delete() -> Vec<String> {
    nat_rule(Op::Delete)
}

/// Program and arguments for the IPv4 mark rule.
pub fn mark_rule() -> Vec<String> {
    rule("iptables", Op::Add)
}

pub fn mark_rule_check() -> Vec<String> {
    rule("iptables", Op::Check)
}

pub fn mark_rule_delete() -> Vec<String> {
    rule("iptables", Op::Delete)
}

/// Program and arguments for the IPv6 drop rule.
pub fn v6_drop_rule() -> Vec<String> {
    v6_rule(Op::Add)
}

pub fn v6_drop_rule_check() -> Vec<String> {
    v6_rule(Op::Check)
}

pub fn v6_drop_rule_delete() -> Vec<String> {
    v6_rule(Op::Delete)
}

enum Op {
    Add,
    Check,
    Delete,
}

impl Op {
    fn flag(&self) -> &'static str {
        match self {
            Op::Add => "-A",
            Op::Check => "-C",
            Op::Delete => "-D",
        }
    }
}

fn rule(_program: &str, op: Op) -> Vec<String> {
    vec![
        "-t".into(),
        "mangle".into(),
        op.flag().into(),
        "OUTPUT".into(),
        "-m".into(),
        "cgroup".into(),
        "--path".into(),
        CGROUP_NAME.into(),
        "-j".into(),
        "MARK".into(),
        "--set-mark".into(),
        format!("0x{MARK:x}"),
    ]
}

fn nat_rule(op: Op) -> Vec<String> {
    vec![
        "-t".into(),
        "nat".into(),
        op.flag().into(),
        "POSTROUTING".into(),
        "-m".into(),
        "mark".into(),
        "--mark".into(),
        format!("0x{MARK:x}"),
        "-j".into(),
        "MASQUERADE".into(),
    ]
}

fn v6_rule(op: Op) -> Vec<String> {
    vec![
        "-t".into(),
        "mangle".into(),
        op.flag().into(),
        "OUTPUT".into(),
        "-m".into(),
        "cgroup".into(),
        "--path".into(),
        CGROUP_NAME.into(),
        "-j".into(),
        "DROP".into(),
    ]
}

/// `ip rule add …`
pub fn rule_add() -> Vec<String> {
    vec![
        "rule".into(),
        "add".into(),
        "fwmark".into(),
        format!("0x{MARK:x}"),
        "lookup".into(),
        TABLE.to_string(),
        "pref".into(),
        RULE_PREF.to_string(),
    ]
}

/// `ip rule del …`
pub fn rule_del() -> Vec<String> {
    vec![
        "rule".into(),
        "del".into(),
        "fwmark".into(),
        format!("0x{MARK:x}"),
        "lookup".into(),
        TABLE.to_string(),
        "pref".into(),
        RULE_PREF.to_string(),
    ]
}

/// `ip route flush table 100`
pub fn table_flush() -> Vec<String> {
    vec![
        "route".into(),
        "flush".into(),
        "table".into(),
        TABLE.to_string(),
    ]
}

/// `ip route add default dev <tunnel> table 100`
pub fn default_route(tunnel: &str) -> Vec<String> {
    vec![
        "route".into(),
        "add".into(),
        "default".into(),
        "dev".into(),
        tunnel.into(),
        "table".into(),
        TABLE.to_string(),
    ]
}

/// Destinations worth copying from the tunnel's main-table routes into
/// [`TABLE`].
///
/// Eddie leaves the tunnel interface configured but with no default route, so
/// table 100 needs a default of its own plus the tunnel's own subnet (OpenVPN
/// installs one, WireGuard is point to point and installs none). The source and
/// metric of the original route are dropped: the kernel derives them again for
/// the copy.
pub fn copied_routes(tunnel: &str, main_routes: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for line in main_routes.lines() {
        let mut fields = line.split_whitespace();
        let Some(dest) = fields.next() else { continue };
        if dest == "default" || !is_plain_destination(dest) {
            continue;
        }
        // Only routes that actually point at the tunnel.
        let dev = line
            .split_whitespace()
            .skip_while(|f| *f != "dev")
            .nth(1)
            .unwrap_or_default();
        if dev != tunnel {
            continue;
        }
        out.push(vec![
            "route".into(),
            "add".into(),
            dest.to_string(),
            "dev".into(),
            tunnel.into(),
            "table".into(),
            TABLE.to_string(),
        ]);
    }
    out
}

/// Reject anything that is not a plain `a.b.c.d`, `a.b.c.d/n` or `::/n` prefix.
/// Destinations come from `ip route` output, so this is belt and braces: it
/// keeps a malformed or hostile route line from reaching the argument list.
fn is_plain_destination(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == '.' || c == ':' || c == '/')
        && value.chars().any(|c| c.is_ascii_digit())
}

/// Teardown order: stop marking, then drop the table the mark pointed at, then
/// the rule that selected it.
pub fn teardown_steps() -> Vec<Vec<String>> {
    vec![table_flush(), rule_del()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_rule_selects_the_cgroup() {
        assert_eq!(
            mark_rule().join(" "),
            "-t mangle -A OUTPUT -m cgroup --path nimbus.apps -j MARK --set-mark 0x4e4d"
        );
        assert_eq!(
            mark_rule_check().join(" "),
            "-t mangle -C OUTPUT -m cgroup --path nimbus.apps -j MARK --set-mark 0x4e4d"
        );
        assert_eq!(
            mark_rule_delete().join(" "),
            "-t mangle -D OUTPUT -m cgroup --path nimbus.apps -j MARK --set-mark 0x4e4d"
        );
    }

    #[test]
    fn marked_traffic_is_source_natted() {
        assert_eq!(
            masquerade_rule().join(" "),
            "-t nat -A POSTROUTING -m mark --mark 0x4e4d -j MASQUERADE"
        );
        assert_eq!(
            masquerade_rule_delete().join(" "),
            "-t nat -D POSTROUTING -m mark --mark 0x4e4d -j MASQUERADE"
        );
        // No `-o`: teardown must not depend on knowing the tunnel device.
        assert!(!masquerade_rule().iter().any(|a| a == "-o"));
        assert!(masquerade_rule_check().iter().any(|a| a == "-C"));
    }

    #[test]
    fn ipv6_from_included_apps_is_dropped() {
        assert_eq!(
            v6_drop_rule().join(" "),
            "-t mangle -A OUTPUT -m cgroup --path nimbus.apps -j DROP"
        );
        assert!(v6_drop_rule_delete().join(" ").contains("-D OUTPUT"));
    }

    #[test]
    fn rule_points_mark_at_table() {
        assert_eq!(
            rule_add().join(" "),
            "rule add fwmark 0x4e4d lookup 100 pref 1000"
        );
        assert_eq!(
            rule_del().join(" "),
            "rule del fwmark 0x4e4d lookup 100 pref 1000"
        );
    }

    #[test]
    fn default_route_targets_the_tunnel() {
        assert_eq!(
            default_route("Eddie").join(" "),
            "route add default dev Eddie table 100"
        );
    }

    #[test]
    fn copies_tunnel_subnet_but_not_other_routes() {
        let main = "\
default via 192.168.4.1 dev enp42s0 proto dhcp src 192.168.6.124 metric 100
1.1.1.1 dev tun0 scope link
10.65.4.0/24 dev tun0 proto kernel scope link src 10.65.4.15
192.168.4.0/22 dev enp42s0 proto kernel scope link src 192.168.6.124 metric 100
198.54.129.123 via 192.168.4.1 dev enp42s0";
        let copied: Vec<String> = copied_routes("tun0", main)
            .into_iter()
            .map(|a| a.join(" "))
            .collect();
        assert_eq!(
            copied,
            vec![
                "route add 1.1.1.1 dev tun0 table 100",
                "route add 10.65.4.0/24 dev tun0 table 100",
            ]
        );
    }

    #[test]
    fn wireguard_has_no_subnet_route_to_copy() {
        let main = "\
default via 192.168.4.1 dev enp42s0 proto dhcp metric 100
1.1.1.1 dev Eddie scope link
198.44.129.2 dev Eddie scope link
198.54.129.117 via 192.168.4.1 dev enp42s0";
        let copied: Vec<String> = copied_routes("Eddie", main)
            .into_iter()
            .map(|a| a.join(" "))
            .collect();
        assert_eq!(
            copied,
            vec![
                "route add 1.1.1.1 dev Eddie table 100",
                "route add 198.44.129.2 dev Eddie table 100",
            ]
        );
    }

    #[test]
    fn teardown_flushes_before_unmarking() {
        let steps: Vec<String> = teardown_steps()
            .into_iter()
            .map(|a| a.join(" "))
            .collect();
        assert_eq!(
            steps,
            vec![
                "route flush table 100",
                "rule del fwmark 0x4e4d lookup 100 pref 1000"
            ]
        );
    }

    #[test]
    fn rejects_malformed_destinations() {
        assert!(is_plain_destination("10.0.0.0/8"));
        assert!(is_plain_destination("2001:db8::/32"));
        assert!(is_plain_destination("1.1.1.1"));
        assert!(!is_plain_destination("default"));
        assert!(!is_plain_destination("$(reboot)"));
        assert!(!is_plain_destination("dev"));
        assert!(!is_plain_destination(""));
    }
}
