//! Address-family adaptation at the UDP socket boundary.
//!
//! A dual-stack listener (`[::]`, which Linux serves both families from unless
//! `bindv6only` is set) reports an IPv4 peer as an IPv4-mapped IPv6 address
//! (`::ffff:a.b.c.d`), and refuses a send to a real `V4` destination. Neither
//! form should leak past this boundary:
//!
//! - **Inbound**: [`canonical`] unmaps, so an IPv4 peer looks like the `V4` it
//!   is. ICE pairing keys off the address family (see
//!   [`pick_local`](crate::session::pick_local)), so without this an IPv4 peer
//!   on a dual-stack socket would be paired against the IPv6 host candidate.
//! - **Outbound**: [`to_family`] re-maps, because the destination str0m hands
//!   back is the canonical address we fed it.

use std::net::{IpAddr, SocketAddr};

/// Unmap an IPv4-mapped peer address to the `V4` it really is; other addresses
/// pass through unchanged.
pub(crate) fn canonical(addr: SocketAddr) -> SocketAddr {
	SocketAddr::new(addr.ip().to_canonical(), addr.port())
}

/// Adapt a destination to what `socket_is_v6`'s socket will accept: an AF_INET6
/// socket can only send to an IPv6 address, so an IPv4 destination has to go
/// back to its mapped form.
pub(crate) fn to_family(dst: SocketAddr, socket_is_v6: bool) -> SocketAddr {
	match (socket_is_v6, dst.ip()) {
		(true, IpAddr::V4(ip)) => SocketAddr::new(IpAddr::V6(ip.to_ipv6_mapped()), dst.port()),
		_ => dst,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn canonical_unmaps_v4_mapped_peers() {
		// What a dual-stack socket reports for an IPv4 peer.
		let mapped: SocketAddr = "[::ffff:1.2.3.4]:5000".parse().unwrap();
		assert_eq!(canonical(mapped), "1.2.3.4:5000".parse().unwrap());
	}

	#[test]
	fn canonical_passes_through_native_addresses() {
		let v4: SocketAddr = "1.2.3.4:5000".parse().unwrap();
		let v6: SocketAddr = "[2001:db8::1]:5000".parse().unwrap();
		assert_eq!(canonical(v4), v4);
		assert_eq!(canonical(v6), v6);
	}

	#[test]
	fn to_family_remaps_v4_for_a_dual_stack_socket() {
		let v4: SocketAddr = "1.2.3.4:5000".parse().unwrap();
		assert_eq!(to_family(v4, true), "[::ffff:1.2.3.4]:5000".parse().unwrap());
	}

	#[test]
	fn to_family_leaves_matching_families_alone() {
		let v4: SocketAddr = "1.2.3.4:5000".parse().unwrap();
		let v6: SocketAddr = "[2001:db8::1]:5000".parse().unwrap();
		assert_eq!(to_family(v4, false), v4);
		assert_eq!(to_family(v6, true), v6);
	}

	/// The round trip a dual-stack server does per IPv4 peer: canonicalize the
	/// source on the way in, then re-map str0m's destination on the way out.
	#[test]
	fn canonical_and_to_family_round_trip() {
		let mapped: SocketAddr = "[::ffff:1.2.3.4]:5000".parse().unwrap();
		assert_eq!(to_family(canonical(mapped), true), mapped);
	}
}
