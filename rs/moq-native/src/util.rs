use anyhow::Context;
use std::net::SocketAddr;

/// Resolve a `host:port` string to a single [`std::net::SocketAddr`],
/// falling back to `default` when `addr` is `None`.
///
/// Accepts both literal socket addresses (e.g. `[::]:443`) and DNS hostnames
/// paired with a port (e.g. `fly-global-services:443`). Only the first
/// resolved address is returned; Quinn only supports a single IP when
/// binding/connecting.
pub(crate) fn resolve(addr: Option<&str>, default: &str) -> anyhow::Result<SocketAddr> {
	use std::net::ToSocketAddrs;
	addr.unwrap_or(default)
		.to_socket_addrs()
		.context("invalid address")?
		.next()
		.context("no addresses resolved")
}

/// Pick a single DNS entry from `addrs`, preferring one whose address family
/// matches `local`. Falls back to the first entry when no family match exists.
///
/// Quinn doesn't support happy eyeballs and the local socket may be bound to a
/// single family (especially on Windows, where IPv6 sockets are not dual-stack
/// by default), so a mismatched DNS entry causes `sendmsg` to fail with
/// `AddrNotAvailable`. See <https://github.com/moq-dev/moq/issues/1375>.
pub(crate) fn pick_addr(addrs: impl IntoIterator<Item = SocketAddr>, local: SocketAddr) -> Option<SocketAddr> {
	let mut first = None;
	let mut matching = None;
	for addr in addrs {
		if first.is_none() {
			first = Some(addr);
		}
		if addr.is_ipv4() == local.is_ipv4() {
			matching = Some(addr);
			break;
		}
	}
	matching.or(first)
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
	fn pick_addr_falls_back_to_first() {
		let v4: SocketAddr = "127.0.0.1:443".parse().unwrap();
		let local_v6: SocketAddr = "[::]:0".parse().unwrap();

		// No IPv6 entry available, fall back to the IPv4 entry.
		assert_eq!(pick_addr([v4], local_v6), Some(v4));
	}

	#[test]
	fn pick_addr_empty() {
		let local: SocketAddr = "0.0.0.0:0".parse().unwrap();
		assert_eq!(pick_addr(std::iter::empty(), local), None);
	}
}
