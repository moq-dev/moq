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

/// Pick a single DNS entry from `addrs`, preferring one whose address family
/// matches `local`. Falls back to the first entry when no family match exists.
///
/// Each entry is normalized to the local socket's family when possible: an
/// IPv4-mapped IPv6 address is unwrapped for an IPv4 socket, and a plain IPv4
/// address is wrapped for an IPv6 socket. Quinn doesn't support happy eyeballs
/// and the local socket may be bound to a single family (especially on
/// Windows, where IPv6 sockets are not dual-stack by default), so a
/// family-mismatched destination causes `sendmsg` to fail with
/// `AddrNotAvailable`. See <https://github.com/moq-dev/moq/issues/1375>.
pub(crate) fn pick_addr(addrs: impl IntoIterator<Item = SocketAddr>, local: SocketAddr) -> Option<SocketAddr> {
	let mut converted = None;
	let mut other = None;
	for addr in addrs {
		// A native family match wins outright.
		if addr.is_ipv4() == local.is_ipv4() {
			return Some(addr);
		}
		let normalized = normalize_family(addr, local);
		if normalized.is_ipv4() == local.is_ipv4() {
			if converted.is_none() {
				converted = Some(normalized);
			}
		} else if other.is_none() {
			other = Some(addr);
		}
	}
	converted.or(other)
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

	#[test]
	fn pick_addr_prefers_matching_family() {
		let v4: SocketAddr = "127.0.0.1:443".parse().unwrap();
		let v6: SocketAddr = "[::1]:443".parse().unwrap();
		let local_v4: SocketAddr = "0.0.0.0:0".parse().unwrap();
		let local_v6: SocketAddr = "[::]:0".parse().unwrap();

		// IPv6 listed first, but local socket is IPv4: pick IPv4.
		assert_eq!(pick_addr([v6, v4], local_v4), Some(v4));
		// IPv4 listed first, but local socket is IPv6: pick IPv6.
		assert_eq!(pick_addr([v4, v6], local_v6), Some(v6));
	}

	#[test]
	fn pick_addr_wraps_v4_for_v6_socket() {
		let v4: SocketAddr = "127.0.0.1:443".parse().unwrap();
		let mapped: SocketAddr = "[::ffff:127.0.0.1]:443".parse().unwrap();
		let local_v6: SocketAddr = "[::]:0".parse().unwrap();

		// IPv6 socket with only an IPv4 DNS entry: wrap as IPv4-mapped IPv6.
		assert_eq!(pick_addr([v4], local_v6), Some(mapped));
	}

	#[test]
	fn pick_addr_unwraps_v4_mapped_for_v4_socket() {
		let mapped: SocketAddr = "[::ffff:127.0.0.1]:443".parse().unwrap();
		let v4: SocketAddr = "127.0.0.1:443".parse().unwrap();
		let local_v4: SocketAddr = "0.0.0.0:0".parse().unwrap();

		// IPv4 socket given an IPv4-mapped IPv6 entry: unwrap to plain IPv4.
		assert_eq!(pick_addr([mapped], local_v4), Some(v4));
	}

	#[test]
	fn pick_addr_falls_back_for_unmappable_v6() {
		let v6: SocketAddr = "[2001:db8::1]:443".parse().unwrap();
		let local_v4: SocketAddr = "0.0.0.0:0".parse().unwrap();

		// IPv4 socket with only a true IPv6 entry: no conversion possible,
		// fall back to the entry as-is so the OS surfaces a clear error.
		assert_eq!(pick_addr([v6], local_v4), Some(v6));
	}

	#[test]
	fn pick_addr_empty() {
		let local: SocketAddr = "0.0.0.0:0".parse().unwrap();
		assert_eq!(pick_addr(std::iter::empty(), local), None);
	}
}
