//! Dual-stack socket binding.
//!
//! Quinn uses a single socket and relies on the OS to route both address
//! families. On Linux an `[::]` socket accepts IPv4 too, but Windows defaults
//! `IPV6_V6ONLY` to on, so an IPv6 socket silently drops every IPv4 packet. The
//! helpers here clear that before binding, so a relay on `[::]` is reachable
//! over IPv4 and a dual-stack client can dial IPv4 servers (via IPv4-mapped
//! addresses; the client's address-family matching lives in `util::pick_addr`).
//! See <https://github.com/moq-dev/moq/issues/1375>.

use socket2::{Domain, Protocol, Socket, Type};
use std::net::{SocketAddr, TcpListener, UdpSocket};

/// Bind a UDP socket, making an IPv6 socket dual-stack so it also serves IPv4.
pub fn udp(addr: SocketAddr) -> std::io::Result<UdpSocket> {
	let domain = if addr.is_ipv4() { Domain::IPV4 } else { Domain::IPV6 };
	let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
	make_dual_stack(&socket, addr);
	socket.bind(&addr.into())?;
	Ok(socket.into())
}

/// Bind a TCP listener, making an IPv6 socket dual-stack so it also serves IPv4.
///
/// The returned listener is non-blocking, ready for
/// [`axum_server::from_tcp`](https://docs.rs/axum-server).
pub fn tcp(addr: SocketAddr) -> std::io::Result<TcpListener> {
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

/// Clear `IPV6_V6ONLY` so an IPv6 socket also accepts IPv4. Best-effort: a
/// platform that rejects the option keeps its default rather than failing the
/// bind. No-op for IPv4 sockets.
fn make_dual_stack(socket: &Socket, addr: SocketAddr) {
	if addr.is_ipv6()
		&& let Err(err) = socket.set_only_v6(false)
	{
		tracing::warn!(%err, "failed to enable dual-stack IPv6 socket; IPv4 clients may be unreachable");
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn udp_ipv6_is_dual_stack() {
		// An IPv6 wildcard bind should come back dual-stack so IPv4 traffic
		// reaches it. socket2 lets us read the option back to confirm.
		let socket = udp("[::]:0".parse().unwrap()).unwrap();
		let socket = Socket::from(socket);
		assert!(!socket.only_v6().unwrap(), "IPv6 socket should be dual-stack");
	}

	#[test]
	fn udp_ipv4_still_binds() {
		let socket = udp("127.0.0.1:0".parse().unwrap()).unwrap();
		assert!(socket.local_addr().unwrap().is_ipv4());
	}

	#[test]
	fn tcp_ipv6_is_dual_stack() {
		let listener = tcp("[::]:0".parse().unwrap()).unwrap();
		let socket = Socket::from(listener);
		assert!(!socket.only_v6().unwrap(), "IPv6 listener should be dual-stack");
	}
}
