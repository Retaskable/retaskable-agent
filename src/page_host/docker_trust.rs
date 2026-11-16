//! Reaching a page from the Windows host while the agent runs in a Docker container (docs/specs/docker.md section 7,
//! agent-settings.md section 4.4). A port Docker publishes on the host's `127.0.0.1` reaches the container from the
//! container's default gateway, never from a loopback address, and the page must listen on `0.0.0.0` inside the
//! container for the publish to work at all. With `RETASKABLE_TRUST_DOCKER_HOST=1` (Linux only) the page binds
//! `0.0.0.0` and counts that one gateway address as "this computer", the same as loopback. Every other peer is
//! treated as before. Without the variable nothing here does anything.
use std::net::{IpAddr, Ipv4Addr};

use crate::env::Env;

pub const TRUST_VAR: &str = "RETASKABLE_TRUST_DOCKER_HOST";
const ROUTE_TABLE: &str = "/proc/net/route";

/// Whether the opt-in is on: the variable is `1`, on Linux.
pub fn enabled(env: &Env) -> bool {
    cfg!(target_os = "linux") && env.get(TRUST_VAR).is_some_and(|v| v.trim() == "1")
}

/// The address a page listens on: every address while network access is on or the Docker opt-in is, else this
/// computer only.
pub fn bind_ip(network_on: bool, env: &Env) -> Ipv4Addr {
    if network_on || enabled(env) {
        Ipv4Addr::UNSPECIFIED
    } else {
        Ipv4Addr::LOCALHOST
    }
}

/// The default route's gateway in a `/proc/net/route` table: the row whose destination is `00000000` with the
/// `RTF_UP` and `RTF_GATEWAY` flags. Addresses there are the kernel's `u32` printed in hex, so little-endian bytes.
pub fn parse_default_gateway(table: &str) -> Option<Ipv4Addr> {
    table.lines().skip(1).find_map(|line| {
        let cols: Vec<&str> = line.split_whitespace().collect();
        let [_iface, dest, gateway, flags, ..] = cols.as_slice() else {
            return None;
        };
        let flags = u32::from_str_radix(flags, 16).ok()?;
        if *dest != "00000000" || flags & 0x3 != 0x3 {
            return None;
        }
        let gateway = u32::from_str_radix(gateway, 16).ok()?;
        (gateway != 0).then(|| Ipv4Addr::from(gateway.to_le_bytes()))
    })
}

/// The gateway to trust as this computer: `None` unless the opt-in is on. With it on but no default route readable
/// nothing is trusted (the page is refused to everyone but loopback, as with network access off) and the log says so.
pub fn gateway(env: &Env) -> Option<IpAddr> {
    gateway_from(env, || std::fs::read_to_string(ROUTE_TABLE))
}

fn gateway_from(env: &Env, read_table: impl FnOnce() -> std::io::Result<String>) -> Option<IpAddr> {
    if !enabled(env) {
        return None;
    }
    match read_table().ok().as_deref().and_then(parse_default_gateway) {
        Some(ip) => {
            tracing::info!(
                "{TRUST_VAR}=1: requests from the container's gateway {ip} count as this computer. Publish the port on the host's 127.0.0.1 only."
            );
            Some(IpAddr::V4(ip))
        }
        None => {
            tracing::error!(
                "{TRUST_VAR}=1, but no default gateway was found in {ROUTE_TABLE}: no one is trusted as this computer."
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
        eth0\t00000000\t0100A8C0\t0003\t0\t0\t0\t00000000\t0\t0\t0\n\
        eth0\t0000A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0\n";

    fn on() -> Env {
        Env::from_pairs([(TRUST_VAR, "1")])
    }

    #[test]
    fn it_reads_the_default_routes_gateway_from_a_route_table() {
        assert_eq!(parse_default_gateway(TABLE), Some(Ipv4Addr::new(192, 168, 0, 1)));
        let docker = "Iface\tDestination\tGateway\tFlags\n\
            eth0\t00000000\t010011AC\t0003\t0\t0\t0\t00000000\t0\t0\t0\n";
        assert_eq!(parse_default_gateway(docker), Some(Ipv4Addr::new(172, 17, 0, 1)));
    }

    #[test]
    fn no_default_route_or_a_down_one_has_no_gateway() {
        assert_eq!(parse_default_gateway(""), None);
        assert_eq!(parse_default_gateway("Iface\tDestination\tGateway\tFlags\n"), None);
        let local_only = "Iface\tDestination\tGateway\tFlags\neth0\t0000A8C0\t00000000\t0001\n";
        assert_eq!(parse_default_gateway(local_only), None);
        let down = "Iface\tDestination\tGateway\tFlags\neth0\t00000000\t0100A8C0\t0002\n";
        assert_eq!(parse_default_gateway(down), None);
    }

    #[test]
    fn the_bind_address_follows_the_flag_and_network_access() {
        assert_eq!(bind_ip(false, &Env::empty()), Ipv4Addr::LOCALHOST);
        assert_eq!(bind_ip(true, &Env::empty()), Ipv4Addr::UNSPECIFIED);
        assert_eq!(
            bind_ip(false, &Env::from_pairs([(TRUST_VAR, "0")])),
            Ipv4Addr::LOCALHOST
        );
        if cfg!(target_os = "linux") {
            assert_eq!(bind_ip(false, &on()), Ipv4Addr::UNSPECIFIED);
        }
    }

    #[test]
    fn the_gateway_is_found_only_with_the_flag_on() {
        let table = || Ok(TABLE.to_string());
        assert_eq!(gateway_from(&Env::empty(), table), None);
        if cfg!(target_os = "linux") {
            assert_eq!(
                gateway_from(&on(), table),
                Some(IpAddr::V4(Ipv4Addr::new(192, 168, 0, 1)))
            );
            assert_eq!(gateway_from(&on(), || Err(std::io::ErrorKind::NotFound.into())), None);
        }
    }
}
