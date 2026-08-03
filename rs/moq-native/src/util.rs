use std::net::{IpAddr, SocketAddr};

/// Resolve a `host:port` string to a single [`std::net::SocketAddr`],
/// falling back to `default` when `addr` is `None`.
///
/// Accepts both literal socket addresses (e.g. `[::]:443`) and DNS hostnames
/// paired with a port (e.g. `fly-global-services:443`). Only the first
/// resolved address is returned; Quinn only supports a single IP when
/// binding/connecting.
pub(crate) fn resolve(addr: Option<&str>, default: &str) -> std::io::Result<SocketAddr> {
	use std::net::ToSocketAddrs;
	addr.unwrap_or(default)
		.to_socket_addrs()?
		.next()
		.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no addresses resolved"))
}

/// Pick a single DNS entry from `addrs`: the first one the local socket can
/// address, keeping the order the resolver returned them in.
///
/// That order is the whole point. `getaddrinfo` has already sorted its answer
/// per RFC 6724, whose first rule demotes a destination the host has no usable
/// source address for, so an unroutable IPv6 record sinks below the IPv4 ones.
/// Reordering on top of that throws away the only reachability signal we have:
/// a socket bound to `[::]` is dual-stack (see [`crate::bind`]) and matches
/// *both* families, so preferring an address-family match would promote every
/// AAAA over every A and hand back the one destination the host can't reach.
/// Quinn doesn't support happy eyeballs, so that single choice is the whole
/// dial: `sendmsg` then fails with `Network is unreachable` for every packet
/// until the handshake times out.
///
/// Each entry is converted to the local socket's family when that's lossless:
/// an IPv4-mapped IPv6 address is unwrapped for an IPv4 socket, and a plain
/// IPv4 address is wrapped for an IPv6 socket. An entry the socket can't send
/// to (see [`addressable`]) is skipped in favor of one it can, which is what a
/// socket bound to a single family needs. `dual_stack` is
/// [`crate::bind::udp_is_dual_stack`] for that socket. Falls back to the first
/// entry when there's no usable one, so the OS reports the failure rather than
/// us inventing one. See <https://github.com/moq-dev/moq/issues/1375>.
pub(crate) fn pick_addr(
	addrs: impl IntoIterator<Item = SocketAddr>,
	local: SocketAddr,
	dual_stack: bool,
) -> Option<SocketAddr> {
	let mut fallback = None;
	for addr in addrs {
		let addr = normalize_family(addr, local);
		if addressable(addr, local, dual_stack) {
			return Some(addr);
		}
		fallback.get_or_insert(addr);
	}
	fallback
}

/// Whether a socket bound to `local` can send to `dest`.
///
/// Mostly this is the address family, but reaching IPv4 from an IPv6 socket has
/// a wrinkle: it means sending to an IPv4-mapped destination, which the kernel
/// turns back into a real IPv4 packet, and that needs an IPv4 source address. So
/// it takes both a socket that is actually dual-stack (`IPV6_V6ONLY` cleared,
/// which [`crate::bind::udp`] only attempts) and a bind that left an IPv4 source
/// to use: `[::]` does, since the kernel picks the source, and an IPv4-mapped
/// bind already is one, but a concrete IPv6 bind is not. The mirror holds too:
/// an IPv4-mapped bind can't reach a real IPv6 destination.
fn addressable(dest: SocketAddr, local: SocketAddr, dual_stack: bool) -> bool {
	let (SocketAddr::V6(dest), SocketAddr::V6(local)) = (dest, local) else {
		return dest.is_ipv4() == local.is_ipv4();
	};

	match (dest.ip().to_ipv4_mapped(), local.ip().to_ipv4_mapped()) {
		(Some(_), None) => dual_stack && local.ip().is_unspecified(),
		(None, Some(_)) => false,
		_ => true,
	}
}

/// Convert `addr` to match the family of `local` when the conversion is
/// lossless: unwrap IPv4-mapped IPv6 to IPv4, or wrap IPv4 as IPv4-mapped IPv6.
fn normalize_family(addr: SocketAddr, local: SocketAddr) -> SocketAddr {
	match (addr, local.is_ipv4()) {
		(SocketAddr::V6(v6), true) => match v6.ip().to_ipv4_mapped() {
			Some(v4) => SocketAddr::new(IpAddr::V4(v4), v6.port()),
			None => addr,
		},
		(SocketAddr::V4(v4), false) => SocketAddr::new(IpAddr::V6(v4.ip().to_ipv6_mapped()), v4.port()),
		_ => addr,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn resolves_socket_literal() {
		let addr = resolve(Some("[::]:0"), "[::]:443").unwrap();
		assert!(addr.ip().is_unspecified());
		assert_eq!(addr.port(), 0);
	}

	#[test]
	fn resolves_dns_hostname() {
		let addr = resolve(Some("localhost:0"), "[::]:443").unwrap();
		assert!(addr.ip().is_loopback());
		assert_eq!(addr.port(), 0);
	}

	#[test]
	fn falls_back_to_default() {
		let addr = resolve(None, "127.0.0.1:1234").unwrap();
		assert_eq!(addr.ip().to_string(), "127.0.0.1");
		assert_eq!(addr.port(), 1234);
	}

	const V4: &str = "192.0.2.1:443";
	const V4_MAPPED: &str = "[::ffff:192.0.2.1]:443";
	const V6: &str = "[2001:db8::1]:443";
	const LOCAL_V4: &str = "0.0.0.0:0";
	const LOCAL_V6: &str = "[::]:0";
	/// `--client-bind` pinned to a concrete source rather than the wildcard.
	const BOUND_V6: &str = "[2001:db8::5]:0";
	const BOUND_V4_MAPPED: &str = "[::ffff:192.0.2.5]:0";

	fn addr(s: &str) -> SocketAddr {
		s.parse().unwrap()
	}

	/// [`pick_addr`] for a socket that came back dual-stack, which is what
	/// [`crate::bind::udp`] produces everywhere it can.
	fn dual_stack(addrs: impl IntoIterator<Item = SocketAddr>, local: SocketAddr) -> Option<SocketAddr> {
		pick_addr(addrs, local, true)
	}

	/// A dual-stack socket can reach both families, so the resolver's ranking is
	/// the only thing that says which destination actually works. A host with no
	/// route to the AAAA gets it ranked last by RFC 6724; preferring an address
	/// family match promoted it back to the front and dialed the one destination
	/// the host couldn't reach.
	#[test]
	fn pick_addr_keeps_the_resolver_ranking() {
		assert_eq!(dual_stack([addr(V4), addr(V6)], addr(LOCAL_V6)), Some(addr(V4_MAPPED)));
		// And the other way around: a host with working IPv6 ranks the AAAA first,
		// so that's what gets dialed.
		assert_eq!(dual_stack([addr(V6), addr(V4)], addr(LOCAL_V6)), Some(addr(V6)));
	}

	/// A socket bound to one family skips what it can't send to, whatever the
	/// ranking, since the alternative is a guaranteed `sendmsg` failure.
	#[test]
	fn pick_addr_skips_a_family_the_socket_cant_send_to() {
		assert_eq!(dual_stack([addr(V6), addr(V4)], addr(LOCAL_V4)), Some(addr(V4)));

		// A bind pinned to a concrete IPv6 source has no IPv4 source to send an
		// IPv4-mapped destination from, so the native IPv6 entry wins even though
		// the resolver ranked the A record first.
		assert_eq!(dual_stack([addr(V4), addr(V6)], addr(BOUND_V6)), Some(addr(V6)));
		// The mirror: an IPv4-mapped bind is an IPv4 socket, so a real IPv6
		// destination is out of reach.
		assert_eq!(
			dual_stack([addr(V6), addr(V4)], addr(BOUND_V4_MAPPED)),
			Some(addr(V4_MAPPED))
		);
	}

	/// `bind::udp` clears `IPV6_V6ONLY` best-effort and keeps the socket when the
	/// platform refuses, warning rather than failing. Such a socket still reads
	/// back as `[::]` but can't send to an IPv4-mapped destination at all, so the
	/// native IPv6 entry has to win.
	#[test]
	fn pick_addr_skips_mapped_ipv4_on_a_v6_only_socket() {
		assert_eq!(pick_addr([addr(V4), addr(V6)], addr(LOCAL_V6), false), Some(addr(V6)));
		// The same socket with a dual-stack option that did take.
		assert_eq!(
			pick_addr([addr(V4), addr(V6)], addr(LOCAL_V6), true),
			Some(addr(V4_MAPPED))
		);
	}

	/// Nothing else to try, so hand back the unusable entry and let the OS report
	/// it. Silently failing the dial would be worse than a clear `sendmsg` error.
	#[test]
	fn pick_addr_still_returns_an_unusable_only_entry() {
		assert_eq!(dual_stack([addr(V4)], addr(BOUND_V6)), Some(addr(V4_MAPPED)));
		assert_eq!(pick_addr([addr(V4)], addr(LOCAL_V6), false), Some(addr(V4_MAPPED)));
	}

	#[test]
	fn pick_addr_wraps_v4_for_v6_socket() {
		// IPv6 socket with only an IPv4 DNS entry: wrap as IPv4-mapped IPv6.
		assert_eq!(dual_stack([addr(V4)], addr(LOCAL_V6)), Some(addr(V4_MAPPED)));
	}

	#[test]
	fn pick_addr_unwraps_v4_mapped_for_v4_socket() {
		// IPv4 socket given an IPv4-mapped IPv6 entry: unwrap to plain IPv4.
		assert_eq!(dual_stack([addr(V4_MAPPED)], addr(LOCAL_V4)), Some(addr(V4)));
	}

	#[test]
	fn pick_addr_falls_back_for_unmappable_v6() {
		// IPv4 socket with only a true IPv6 entry: no conversion possible, so hand
		// it back as-is and let the OS surface a clear error.
		assert_eq!(dual_stack([addr(V6)], addr(LOCAL_V4)), Some(addr(V6)));
	}

	#[test]
	fn pick_addr_empty() {
		assert_eq!(dual_stack(std::iter::empty(), addr(LOCAL_V4)), None);
	}
}
