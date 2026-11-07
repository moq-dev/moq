use crate::Candidate;
use crate::CandidateKind;

/// Standard local preference calculation for a candidate.
///
/// This is the default local preference calculation for a candidate.
///
/// It is used to determine the preference of a candidate when there are multiple candidates
/// for a particular component for a particular data stream.
///
/// The preference is calculated based on the candidate type, the IP version of the candidate,
/// and the same_kind counter.
pub fn default_local_preference(c: &Candidate, same_kind: usize) -> u32 {
    let ip = c.addr();

    // https://datatracker.ietf.org/doc/html/rfc8445#section-5.1.2.1
    // The local preference MUST be an integer from 0 (lowest preference) to
    // 65535 (highest preference) inclusive.  When there is only a single IP
    // address, this value SHOULD be set to 65535.  If there are multiple
    // candidates for a particular component for a particular data stream
    // that have the same type, the local preference MUST be unique for each
    // one.
    // ...
    // If an ICE agent is multihomed and has multiple IP addresses, the
    // recommendations in [RFC8421] SHOULD be followed.  If multiple TURN
    // servers are used, local priorities for the candidates obtained from
    // the TURN servers are chosen in a similar fashion as for multihomed
    // local candidates: the local preference value is used to indicate a
    // preference among different servers, but the preference MUST be unique
    // for each one.
    // ================
    //
    // The above presupposes that we know all the candidates when we start
    // the ice agent. That doesn't work for us, so we deliberately do not
    // follow spec. We assign the following intervals for the different
    // types of candidates:
    //
    // 0     - 16384 => relay
    // 16384 - 32768 => srflx
    // 32768 - 49152 => prflx
    // 49152 - 65536 => host
    //
    // And furthermore we subdivide these to interleave IPv6 with IPv4
    // so that odd numbers are ipv6 and even are ipv4.
    //
    // For host candidates this means:
    // 65535 - first ipv6
    // 65534 - first ipv4
    // 65533 - second ipv6
    // 65432 - second ipv4
    let counter_start: u32 = {
        use CandidateKind::*;
        let x = match c.kind() {
            Host => 65_535,
            PeerReflexive => 49_151,
            ServerReflexive => 32_767,
            Relayed => 16_383,
        };
        x - if ip.is_ipv6() { 0 } else { 1 }
    };

    // For relayed candidates, we add a "punishment" to the local preference
    // if the base address differs in the IP version from the allocated address
    // of the candidate.
    // This punishment ensures that we prefer relayed within the same IP version,
    // e.g. IPv4 <> IPv4 over ones that translate between IP version, e.g. IPv4 <> IPv6.
    let relay_across_ip_version_punishment = if c.kind() == CandidateKind::Relayed {
        if c.local().is_ipv4() != ip.is_ipv4() {
            1000
        } else {
            0
        }
    } else {
        0
    };

    counter_start - same_kind as u32 * 2 - relay_across_ip_version_punishment
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Candidate;
    use std::net::SocketAddr;

    fn ipv4_addr(ip: &str) -> SocketAddr {
        format!("{}:1234", ip).parse().unwrap()
    }

    fn ipv6_addr(ip: &str) -> SocketAddr {
        format!("[{}]:1234", ip).parse().unwrap()
    }

    #[test]
    fn test_host_candidates_ipv4() {
        let addr = ipv4_addr("192.168.1.1");
        let candidate = Candidate::host(addr, "udp").unwrap();

        // First host candidate (same_kind = 0)
        let pref = default_local_preference(&candidate, 0);
        assert_eq!(pref, 65534); // 65535 - 1 (IPv4) - 0 * 2

        // Second host candidate (same_kind = 1)
        let pref = default_local_preference(&candidate, 1);
        assert_eq!(pref, 65532); // 65535 - 1 (IPv4) - 1 * 2

        // Third host candidate (same_kind = 2)
        let pref = default_local_preference(&candidate, 2);
        assert_eq!(pref, 65530); // 65535 - 1 (IPv4) - 2 * 2
    }

    #[test]
    fn test_host_candidates_ipv6() {
        let addr = ipv6_addr("2001:db8::1");
        let candidate = Candidate::host(addr, "udp").unwrap();

        // First host candidate (same_kind = 0)
        let pref = default_local_preference(&candidate, 0);
        assert_eq!(pref, 65535); // 65535 - 0 (IPv6) - 0 * 2

        // Second host candidate (same_kind = 1)
        let pref = default_local_preference(&candidate, 1);
        assert_eq!(pref, 65533); // 65535 - 0 (IPv6) - 1 * 2

        // Third host candidate (same_kind = 2)
        let pref = default_local_preference(&candidate, 2);
        assert_eq!(pref, 65531); // 65535 - 0 (IPv6) - 2 * 2
    }

    #[test]
    fn test_server_reflexive_candidates_ipv4() {
        let addr = ipv4_addr("203.0.113.1");
        let base = ipv4_addr("192.168.1.1");
        let candidate = Candidate::server_reflexive(addr, base, "udp").unwrap();

        // First srflx candidate (same_kind = 0)
        let pref = default_local_preference(&candidate, 0);
        assert_eq!(pref, 32766); // 32767 - 1 (IPv4) - 0 * 2

        // Second srflx candidate (same_kind = 1)
        let pref = default_local_preference(&candidate, 1);
        assert_eq!(pref, 32764); // 32767 - 1 (IPv4) - 1 * 2
    }

    #[test]
    fn test_server_reflexive_candidates_ipv6() {
        let addr = ipv6_addr("2001:db8::100");
        let base = ipv6_addr("2001:db8::1");
        let candidate = Candidate::server_reflexive(addr, base, "udp").unwrap();

        // First srflx candidate (same_kind = 0)
        let pref = default_local_preference(&candidate, 0);
        assert_eq!(pref, 32767); // 32767 - 0 (IPv6) - 0 * 2

        // Second srflx candidate (same_kind = 1)
        let pref = default_local_preference(&candidate, 1);
        assert_eq!(pref, 32765); // 32767 - 0 (IPv6) - 1 * 2
    }

    #[test]
    fn test_peer_reflexive_candidates_ipv4() {
        let addr = ipv4_addr("198.51.100.1");
        let base = ipv4_addr("192.168.1.1");
        let candidate = Candidate::test_peer_rflx(addr, base, "udp");

        // First prflx candidate (same_kind = 0)
        let pref = default_local_preference(&candidate, 0);
        assert_eq!(pref, 49150); // 49151 - 1 (IPv4) - 0 * 2

        // Second prflx candidate (same_kind = 1)
        let pref = default_local_preference(&candidate, 1);
        assert_eq!(pref, 49148); // 49151 - 1 (IPv4) - 1 * 2
    }

    #[test]
    fn test_peer_reflexive_candidates_ipv6() {
        let addr = ipv6_addr("2001:db8::200");
        let base = ipv6_addr("2001:db8::1");
        let candidate = Candidate::test_peer_rflx(addr, base, "udp");

        // First prflx candidate (same_kind = 0)
        let pref = default_local_preference(&candidate, 0);
        assert_eq!(pref, 49151); // 49151 - 0 (IPv6) - 0 * 2

        // Second prflx candidate (same_kind = 1)
        let pref = default_local_preference(&candidate, 1);
        assert_eq!(pref, 49149); // 49151 - 0 (IPv6) - 1 * 2
    }

    #[test]
    fn test_relayed_candidates_ipv4_same_ip_version() {
        let addr = ipv4_addr("192.0.2.1");
        let local = ipv4_addr("192.168.1.1");
        let candidate = Candidate::relayed(addr, local, "udp").unwrap();

        // First relay candidate (same_kind = 0, no IP version punishment)
        let pref = default_local_preference(&candidate, 0);
        assert_eq!(pref, 16382); // 16383 - 1 (IPv4) - 0 * 2 - 0 (no punishment)

        // Second relay candidate (same_kind = 1, no IP version punishment)
        let pref = default_local_preference(&candidate, 1);
        assert_eq!(pref, 16380); // 16383 - 1 (IPv4) - 1 * 2 - 0 (no punishment)
    }

    #[test]
    fn test_relayed_candidates_ipv6_same_ip_version() {
        let addr = ipv6_addr("2001:db8::300");
        let local = ipv6_addr("2001:db8::1");
        let candidate = Candidate::relayed(addr, local, "udp").unwrap();

        // First relay candidate (same_kind = 0, no IP version punishment)
        let pref = default_local_preference(&candidate, 0);
        assert_eq!(pref, 16383); // 16383 - 0 (IPv6) - 0 * 2 - 0 (no punishment)

        // Second relay candidate (same_kind = 1, no IP version punishment)
        let pref = default_local_preference(&candidate, 1);
        assert_eq!(pref, 16381); // 16383 - 0 (IPv6) - 1 * 2 - 0 (no punishment)
    }

    #[test]
    fn test_relayed_candidates_cross_ip_version_punishment() {
        // IPv4 allocated address with IPv6 local address
        let addr = ipv4_addr("192.0.2.1");
        let local = ipv6_addr("2001:db8::1");
        let candidate = Candidate::relayed(addr, local, "udp").unwrap();

        // First relay candidate with IP version punishment
        let pref = default_local_preference(&candidate, 0);
        assert_eq!(pref, 15382); // 16383 - 1 (IPv4) - 0 * 2 - 1000 (punishment)

        // Second relay candidate with IP version punishment
        let pref = default_local_preference(&candidate, 1);
        assert_eq!(pref, 15380); // 16383 - 1 (IPv4) - 1 * 2 - 1000 (punishment)
    }

    #[test]
    fn test_relayed_candidates_cross_ip_version_punishment_reverse() {
        // IPv6 allocated address with IPv4 local address
        let addr = ipv6_addr("2001:db8::300");
        let local = ipv4_addr("192.168.1.1");
        let candidate = Candidate::relayed(addr, local, "udp").unwrap();

        // First relay candidate with IP version punishment
        let pref = default_local_preference(&candidate, 0);
        assert_eq!(pref, 15383); // 16383 - 0 (IPv6) - 0 * 2 - 1000 (punishment)

        // Second relay candidate with IP version punishment
        let pref = default_local_preference(&candidate, 1);
        assert_eq!(pref, 15381); // 16383 - 0 (IPv6) - 1 * 2 - 1000 (punishment)
    }

    #[test]
    fn test_candidate_type_ordering() {
        // Test that different candidate types maintain proper ordering
        let ipv4_addr = ipv4_addr("192.168.1.1");
        let ipv6_addr = ipv6_addr("2001:db8::1");
        let local = ipv4_addr;

        let host_ipv4 = Candidate::host(ipv4_addr, "udp").unwrap();
        let host_ipv6 = Candidate::host(ipv6_addr, "udp").unwrap();
        let srflx_ipv4 = Candidate::server_reflexive(ipv4_addr, local, "udp").unwrap();
        let srflx_ipv6 = Candidate::server_reflexive(ipv6_addr, ipv6_addr, "udp").unwrap();
        let prflx_ipv4 = Candidate::test_peer_rflx(ipv4_addr, local, "udp");
        let prflx_ipv6 = Candidate::test_peer_rflx(ipv6_addr, ipv6_addr, "udp");
        let relay_ipv4 = Candidate::relayed(ipv4_addr, local, "udp").unwrap();
        let relay_ipv6 = Candidate::relayed(ipv6_addr, ipv6_addr, "udp").unwrap();

        let same_kind = 0;

        let host_ipv4_pref = default_local_preference(&host_ipv4, same_kind);
        let host_ipv6_pref = default_local_preference(&host_ipv6, same_kind);
        let srflx_ipv4_pref = default_local_preference(&srflx_ipv4, same_kind);
        let srflx_ipv6_pref = default_local_preference(&srflx_ipv6, same_kind);
        let prflx_ipv4_pref = default_local_preference(&prflx_ipv4, same_kind);
        let prflx_ipv6_pref = default_local_preference(&prflx_ipv6, same_kind);
        let relay_ipv4_pref = default_local_preference(&relay_ipv4, same_kind);
        let relay_ipv6_pref = default_local_preference(&relay_ipv6, same_kind);

        // Host candidates should have highest preference
        assert!(host_ipv4_pref > srflx_ipv4_pref);
        assert!(host_ipv6_pref > srflx_ipv6_pref);
        assert!(host_ipv4_pref > prflx_ipv4_pref);
        assert!(host_ipv6_pref > prflx_ipv6_pref);
        assert!(host_ipv4_pref > relay_ipv4_pref);
        assert!(host_ipv6_pref > relay_ipv6_pref);

        // Peer reflexive candidates should have higher preference than server reflexive
        assert!(prflx_ipv4_pref > srflx_ipv4_pref);
        assert!(prflx_ipv6_pref > srflx_ipv6_pref);

        // Server reflexive candidates should have higher preference than relayed
        assert!(srflx_ipv4_pref > relay_ipv4_pref);
        assert!(srflx_ipv6_pref > relay_ipv6_pref);

        // Peer reflexive candidates should have higher preference than relayed
        assert!(prflx_ipv4_pref > relay_ipv4_pref);
        assert!(prflx_ipv6_pref > relay_ipv6_pref);

        // IPv6 should have higher preference than IPv4 within the same type
        assert!(host_ipv6_pref > host_ipv4_pref);
        assert!(srflx_ipv6_pref > srflx_ipv4_pref);
        assert!(prflx_ipv6_pref > prflx_ipv4_pref);
        assert!(relay_ipv6_pref > relay_ipv4_pref);
    }

    #[test]
    fn test_ipv4_ipv6_interleaving() {
        // Test that IPv6 gets odd numbers and IPv4 gets even numbers
        let ipv4_addr = ipv4_addr("192.168.1.1");
        let ipv6_addr = ipv6_addr("2001:db8::1");

        let host_ipv4 = Candidate::host(ipv4_addr, "udp").unwrap();
        let host_ipv6 = Candidate::host(ipv6_addr, "udp").unwrap();

        let ipv4_pref = default_local_preference(&host_ipv4, 0);
        let ipv6_pref = default_local_preference(&host_ipv6, 0);

        // IPv4 should get even number (65534), IPv6 should get odd number (65535)
        assert_eq!(ipv4_pref % 2, 0, "IPv4 preference should be even");
        assert_eq!(ipv6_pref % 2, 1, "IPv6 preference should be odd");
        assert_eq!(
            ipv6_pref - ipv4_pref,
            1,
            "IPv6 should be exactly 1 higher than IPv4"
        );
    }

    #[test]
    fn test_same_kind_counter_effect() {
        let addr = ipv4_addr("192.168.1.1");
        let candidate = Candidate::host(addr, "udp").unwrap();

        // Test that same_kind counter decreases preference by 2 each time
        let pref0 = default_local_preference(&candidate, 0);
        let pref1 = default_local_preference(&candidate, 1);
        let pref2 = default_local_preference(&candidate, 2);
        let pref3 = default_local_preference(&candidate, 3);

        assert_eq!(
            pref0 - pref1,
            2,
            "Preference should decrease by 2 for each same_kind increment"
        );
        assert_eq!(
            pref1 - pref2,
            2,
            "Preference should decrease by 2 for each same_kind increment"
        );
        assert_eq!(
            pref2 - pref3,
            2,
            "Preference should decrease by 2 for each same_kind increment"
        );
    }
}
