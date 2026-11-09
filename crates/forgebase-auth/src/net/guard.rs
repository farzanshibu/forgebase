//! The SSRF address table shared by every outbound client (FB-60, FB-80):
//! functions `fetch`, queue push webhooks and the auth service's HTTPS client
//! (OAuth/OIDC, SMS, Turnstile).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Why an address may not be contacted (`None`: a public unicast address).
pub fn blocked_reason(ip: IpAddr) -> Option<&'static str> {
    match ip {
        IpAddr::V4(v4) => blocked_v4(v4),
        IpAddr::V6(v6) => blocked_v6(v6),
    }
}

fn blocked_v4(ip: Ipv4Addr) -> Option<&'static str> {
    let o = ip.octets();
    let reason = match o {
        [0, ..] => "the unspecified/\"this network\" range 0.0.0.0/8",
        [10, ..] => "the private range 10.0.0.0/8",
        [100, b, ..] if (64..128).contains(&b) => "the carrier-grade NAT range 100.64.0.0/10",
        [127, ..] => "the loopback range 127.0.0.0/8",
        [169, 254, 169, 254] => "the cloud metadata address 169.254.169.254",
        [169, 254, ..] => "the link-local range 169.254.0.0/16",
        [172, b, ..] if (16..32).contains(&b) => "the private range 172.16.0.0/12",
        [192, 0, 0, _] => "the IETF protocol range 192.0.0.0/24",
        [192, 0, 2, _] | [198, 51, 100, _] | [203, 0, 113, _] => "a documentation range",
        [192, 88, 99, _] => "the 6to4 relay range 192.88.99.0/24",
        [192, 168, ..] => "the private range 192.168.0.0/16",
        [198, b, ..] if b == 18 || b == 19 => "the benchmarking range 198.18.0.0/15",
        [a, ..] if (224..240).contains(&a) => "the multicast range 224.0.0.0/4",
        [a, ..] if a >= 240 => "the reserved range 240.0.0.0/4",
        _ => return None,
    };
    Some(reason)
}

fn blocked_v6(ip: Ipv6Addr) -> Option<&'static str> {
    let s = ip.segments();
    if ip.is_unspecified() {
        return Some("the unspecified address ::");
    }
    if ip.is_loopback() {
        return Some("the IPv6 loopback address ::1");
    }
    // IPv4 inside IPv6: mapped (::ffff:a.b.c.d), compatible (::a.b.c.d),
    // NAT64 (64:ff9b::/96 and 64:ff9b:1::/48), 6to4 (2002::/16).
    if let Some(v4) = ip.to_ipv4_mapped() {
        return blocked_v4(v4).map(|_| "an IPv4-mapped IPv6 address of a blocked IPv4 range");
    }
    if s[..6] == [0, 0, 0, 0, 0, 0] {
        let v4 = Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
        return blocked_v4(v4).map(|_| "an IPv4-compatible IPv6 address of a blocked IPv4 range");
    }
    if s[0] == 0x64 && s[1] == 0xff9b {
        let v4 = Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
        if blocked_v4(v4).is_some() || s[2] == 1 {
            return Some("a NAT64 address of a blocked IPv4 range");
        }
    }
    if s[0] == 0x2002 {
        let v4 = Ipv4Addr::new((s[1] >> 8) as u8, s[1] as u8, (s[2] >> 8) as u8, s[2] as u8);
        if blocked_v4(v4).is_some() {
            return Some("a 6to4 address of a blocked IPv4 range");
        }
    }
    let reason = match s[0] {
        x if x & 0xfe00 == 0xfc00 => "the unique-local range fc00::/7",
        x if x & 0xffc0 == 0xfe80 => "the link-local range fe80::/10",
        x if x & 0xffc0 == 0xfec0 => "the site-local range fec0::/10",
        x if x & 0xff00 == 0xff00 => "the multicast range ff00::/8",
        0x0100 if s[1..4] == [0, 0, 0] => "the discard range 100::/64",
        0x2001 if s[1] == 0x0db8 => "the documentation range 2001:db8::/32",
        0x2001 if s[1] == 0 => "the Teredo range 2001::/32",
        0x2001 if s[1] < 0x0200 => "the IETF protocol range 2001::/23",
        0x3fff if s[1] & 0xf000 == 0 => "the documentation range 3fff::/20",
        _ => return None,
    };
    Some(reason)
}

