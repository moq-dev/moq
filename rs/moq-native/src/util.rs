use std::net::{IpAddr, SocketAddr, TcpListener, UdpSocket};

/// Clear `IPV6_V6ONLY` on an IPv6 socket so it also serves IPv4.
///
/// On Linux an `[::]` socket accepts IPv4 too, but Windows defaults this option
/// to on, so an IPv6 socket silently drops every IPv4 packet. Best-effort: a
/// platform that rejects the option keeps its default rather than failing the
/// bind. No-op for IPv4 sockets. See <https://github.com/moq-dev/moq/issues/1375>.
fn make_dual_stack(socket: &socket2::Socket, addr: SocketAddr) {
	if addr.is_ipv6()
		&& let Err(err) = socket.set_only_v6(false)
	{
		tracing::warn!(%err, "failed to enable dual-stack IPv6 socket; IPv4 clients may be unreachable");
	}
}

/// Bind a UDP socket, making IPv6 sockets dual-stack so they also serve IPv4.
///
/// Quinn uses a single socket and relies on the OS to route both address
/// families, so a relay on `[::]` is reachable over IPv4 and a client on `[::]`
/// can dial IPv4 servers (via IPv4-mapped addresses; see [`pick_addr`]). See
/// [`make_dual_stack`] for the Windows rationale.
pub(crate) fn bind_udp(addr: SocketAddr) -> std::io::Result<UdpSocket> {
	use socket2::{Domain, Protocol, Socket, Type};

	let domain = if addr.is_ipv4() { Domain::IPV4 } else { Domain::IPV6 };
	let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
	make_dual_stack(&socket, addr);
	socket.bind(&addr.into())?;
	Ok(socket.into())
}

/// Bind a TCP listener, making IPv6 sockets dual-stack so they also serve IPv4.
///
/// Mirrors [`bind_udp`] for the relay's HTTP/HTTPS listeners (cert fingerprint
/// and WebSocket fallback), which `axum_server` otherwise binds single-stack and
/// would leave unreachable over IPv4 on Windows. The returned listener is
/// non-blocking, ready for [`axum_server::from_tcp`](https://docs.rs/axum-server).
pub fn bind_tcp(addr: SocketAddr) -> std::io::Result<TcpListener> {
	use socket2::{Domain, Protocol, Socket, Type};

	let domain = if addr.is_ipv4() { Domain::IPV4 } else { Domain::IPV6 };
	let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
	make_dual_stack(&socket, addr);
	// Match std's TcpListener, which sets SO_REUSEADDR on Unix (not Windows) so a
	// restarted relay can rebind a port still in TIME_WAIT.
	#[cfg(not(windows))]
	socket.set_reuse_address(true)?;
	socket.bind(&addr.into())?;
	socket.listen(1024)?;
	let listener: TcpListener = socket.into();
	listener.set_nonblocking(true)?;
	Ok(listener)
}

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
	fn bind_udp_ipv6_is_dual_stack() {
		// An IPv6 wildcard bind should come back dual-stack so IPv4 traffic
		// reaches it. socket2 lets us read the option back to confirm.
		let socket = bind_udp("[::]:0".parse().unwrap()).unwrap();
		let socket = socket2::Socket::from(socket);
		assert!(!socket.only_v6().unwrap(), "IPv6 socket should be dual-stack");
	}

	#[test]
	fn bind_udp_ipv4_still_binds() {
		let socket = bind_udp("127.0.0.1:0".parse().unwrap()).unwrap();
		assert!(socket.local_addr().unwrap().is_ipv4());
	}

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
