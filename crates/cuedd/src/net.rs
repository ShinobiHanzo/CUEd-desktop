//! Which addresses a phone on the same network can reach us at.

use std::net::IpAddr;

/// Addresses a phone can reach us at: site-local IPv4 first, Wi-Fi before
/// Ethernet (the phone is almost always on Wi-Fi). A box with no private
/// address (a VPS, a container) advertises its other IPv4 addresses, and a
/// box with none at all advertises loopback so the UI still works.
/// `CUED_ADVERTISE` (comma-separated IPs or hosts) replaces the list.
pub fn lan_ipv4() -> Vec<IpAddr> {
    let all: Vec<(u8, IpAddr, bool)> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .filter_map(|i| match i.ip() {
            IpAddr::V4(a) if !a.is_link_local() && !a.is_unspecified() => Some((rank(&i.name), IpAddr::V4(a), a.is_private())),
            _ => None,
        })
        .collect();
    let mut v: Vec<(u8, IpAddr)> = all.iter().filter(|(_, _, private)| *private).map(|(r, a, _)| (*r, *a)).collect();
    if v.is_empty() {
        v = all.iter().filter(|(r, _, _)| *r < 9).map(|(r, a, _)| (*r, *a)).collect();
    }
    v.sort_by_key(|(r, _)| *r);
    v.dedup_by_key(|(_, a)| *a);
    let mut out: Vec<IpAddr> = v.into_iter().map(|(_, a)| a).collect();
    if out.is_empty() {
        out.push(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
    }
    out
}

/// Hosts to put in links: `CUED_ADVERTISE` when set, else [`lan_ipv4`] as strings.
pub fn advertised_hosts() -> Vec<String> {
    if let Ok(v) = std::env::var("CUED_ADVERTISE") {
        let hosts: Vec<String> = v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        if !hosts.is_empty() {
            return hosts;
        }
    }
    lan_ipv4().into_iter().map(|a| a.to_string()).collect()
}

fn rank(name: &str) -> u8 {
    let n = name.to_ascii_lowercase();
    if n.starts_with("wl") || n.contains("wi-fi") || n.contains("wifi") {
        0
    } else if n.starts_with("en") || n.starts_with("eth") || n.contains("ethernet") {
        1
    } else if n.starts_with("docker") || n.starts_with("br-") || n.starts_with("veth") || n.starts_with("virbr") {
        9
    } else {
        5
    }
}
