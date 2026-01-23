use std::net::IpAddr;

/// Check if an IP address is safe for outbound connections.
///
/// Rejects loopback, private (RFC 1918), link-local, carrier-grade NAT,
/// IPv4-mapped IPv6 addresses that map to unsafe IPs, multicast,
/// unspecified, and deprecated site-local IPv6.
pub fn is_safe_ip(ip: &IpAddr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return false;
    }

    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            // 10.0.0.0/8
            if octets[0] == 10 {
                return false;
            }
            // 172.16.0.0/12
            if octets[0] == 172 && (16..=31).contains(&octets[1]) {
                return false;
            }
            // 192.168.0.0/16
            if octets[0] == 192 && octets[1] == 168 {
                return false;
            }
            // 169.254.0.0/16 (link-local, includes AWS metadata 169.254.169.254)
            if octets[0] == 169 && octets[1] == 254 {
                return false;
            }
            // 100.64.0.0/10 (carrier-grade NAT)
            if octets[0] == 100 && (64..=127).contains(&octets[1]) {
                return false;
            }
            true
        }
        IpAddr::V6(v6) => {
            // IPv4-mapped (::ffff:0:0/96) — canonicalize to IPv4 and re-check
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_safe_ip(&IpAddr::V4(v4));
            }

            let segments = v6.segments();
            // Unique Local (fc00::/7)
            if (segments[0] & 0xfe00) == 0xfc00 {
                return false;
            }
            // Link-local (fe80::/10)
            if (segments[0] & 0xffc0) == 0xfe80 {
                return false;
            }
            // Site-local (fec0::/10) — deprecated but block anyway
            if (segments[0] & 0xffc0) == 0xfec0 {
                return false;
            }

            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_ipv4_mapped_loopback() {
        let ip: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        assert!(!is_safe_ip(&ip));
    }

    #[test]
    fn rejects_ipv4_mapped_private() {
        assert!(!is_safe_ip(&"::ffff:10.0.0.1".parse().unwrap()));
        assert!(!is_safe_ip(&"::ffff:192.168.1.1".parse().unwrap()));
        assert!(!is_safe_ip(&"::ffff:172.16.0.1".parse().unwrap()));
    }

    #[test]
    fn rejects_ipv4_mapped_link_local() {
        assert!(!is_safe_ip(&"::ffff:169.254.169.254".parse().unwrap()));
    }

    #[test]
    fn rejects_multicast() {
        assert!(!is_safe_ip(&"ff02::1".parse().unwrap()));
    }

    #[test]
    fn rejects_site_local() {
        assert!(!is_safe_ip(&"fec0::1".parse().unwrap()));
    }

    #[test]
    fn rejects_private_ipv4() {
        assert!(!is_safe_ip(&"10.0.0.1".parse().unwrap()));
        assert!(!is_safe_ip(&"192.168.1.1".parse().unwrap()));
        assert!(!is_safe_ip(&"172.16.0.1".parse().unwrap()));
        assert!(!is_safe_ip(&"100.64.0.1".parse().unwrap()));
    }

    #[test]
    fn rejects_loopback() {
        assert!(!is_safe_ip(&"127.0.0.1".parse().unwrap()));
        assert!(!is_safe_ip(&"::1".parse().unwrap()));
    }

    #[test]
    fn rejects_unspecified() {
        assert!(!is_safe_ip(&"0.0.0.0".parse().unwrap()));
        assert!(!is_safe_ip(&"::".parse().unwrap()));
    }

    #[test]
    fn allows_public_ipv4() {
        assert!(is_safe_ip(&"8.8.8.8".parse().unwrap()));
        assert!(is_safe_ip(&"1.1.1.1".parse().unwrap()));
    }

    #[test]
    fn allows_public_ipv6() {
        assert!(is_safe_ip(&"2606:4700:4700::1111".parse().unwrap()));
    }

    #[test]
    fn rejects_carrier_grade_nat_boundary() {
        // 100.64.0.0 - start of range
        assert!(!is_safe_ip(&"100.64.0.0".parse().unwrap()));
        // 100.127.255.255 - end of range
        assert!(!is_safe_ip(&"100.127.255.255".parse().unwrap()));
        // 100.128.0.0 - just outside range
        assert!(is_safe_ip(&"100.128.0.0".parse().unwrap()));
    }

    #[test]
    fn rejects_unique_local_ipv6() {
        assert!(!is_safe_ip(&"fc00::1".parse().unwrap()));
        assert!(!is_safe_ip(&"fd00::1".parse().unwrap()));
    }
}
