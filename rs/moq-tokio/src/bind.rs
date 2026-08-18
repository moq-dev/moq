//! Dual-stack socket binding.
//!
//! Quinn uses a single socket and relies on the OS to route both address
//! families. On Linux an `[::]` socket accepts IPv4 too, but Windows defaults
//! `IPV6_V6ONLY` to on, so an IPv6 socket silently drops every IPv4 packet. The
//! helpers here clear that before binding, so a relay on `[::]` is reachable
//! over IPv4 and a dual-stack client can dial IPv4 servers (via IPv4-mapped
//! addresses; the client's address-family matching lives in
//! `resolve::Candidates::with_local`).
//! See <https://github.com/moq-dev/moq/issues/1375>.

use socket2::{Domain, Protocol, Socket, TcpKeepalive, Type};
use std::io;
use std::net::{SocketAddr, TcpListener, UdpSocket};
use std::sync::Once;
use std::time::Duration;

/// TCP keepalive idle period before the kernel starts probing a silent peer, and
/// the interval between probes. A long-lived connection (a parked WebSocket, an
/// idle HTTP/2 session) can otherwise sit in a `read` forever, so a peer that
/// vanished without a FIN/RST (a yanked cable, a crashed NAT) would pin its
/// socket and any resources behind it. Keepalive lets the kernel surface the dead
/// peer as a read error and tear the connection down. The values are generous
/// enough not to disturb a healthy but momentarily quiet connection.
const KEEPALIVE_IDLE: Duration = Duration::from_secs(30);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);

/// UDP socket buffer size requested in each direction, in bytes.
///
/// A QUIC stack absorbs bursts in the kernel socket buffer: whatever doesn't fit
/// while the process is off the CPU is dropped before quinn ever sees it, and
/// congestion control reads those drops as congestion. The OS defaults are sized
/// for a chatty TCP-era socket (208 KiB on Linux), which a single relay socket
/// carrying every connection blows through in milliseconds. 8 MiB is roughly 64ms
/// of a saturated 1Gbps link, generous next to a scheduler delay and cheap next to
/// what a relay already spends per connection. quic-go asks for 7 MiB.
///
/// Compiled in rather than derived from the NIC: the buffer is a ceiling on queued
/// bytes rather than an allocation, so an oversized request costs an idle socket
/// nothing, while a link's nominal speed says little about the path it feeds (a
/// VM's virtio NIC reports 10Gbps through a 100Mbps uplink).
const UDP_BUFFER: usize = 8 * 1024 * 1024;

/// The sysctl capping `SO_RCVBUF`, named in the warning so an operator knows what
/// to raise. `None` on platforms that size socket buffers per socket only.
#[cfg(any(target_os = "linux", target_os = "android"))]
const RECV_SYSCTL: Option<&str> = Some("net.core.rmem_max");
#[cfg(any(
	target_vendor = "apple",
	target_os = "freebsd",
	target_os = "netbsd",
	target_os = "openbsd"
))]
const RECV_SYSCTL: Option<&str> = Some("kern.ipc.maxsockbuf");
#[cfg(not(any(
	target_os = "linux",
	target_os = "android",
	target_vendor = "apple",
	target_os = "freebsd",
	target_os = "netbsd",
	target_os = "openbsd"
)))]
const RECV_SYSCTL: Option<&str> = None;

/// The sysctl capping `SO_SNDBUF`. See [`RECV_SYSCTL`]; the BSDs cap both
/// directions with the same knob.
#[cfg(any(target_os = "linux", target_os = "android"))]
const SEND_SYSCTL: Option<&str> = Some("net.core.wmem_max");
#[cfg(not(any(target_os = "linux", target_os = "android")))]
const SEND_SYSCTL: Option<&str> = RECV_SYSCTL;

/// What to bind a UDP socket to, and how.
///
/// [`Udp::new`] takes the address and leaves everything else at the platform
/// defaults; reach for the `with_*` methods for the rest.
#[derive(Clone, Copy, Debug)]
pub struct Udp {
	addr: SocketAddr,
	reuse_port: bool,
}

impl Udp {
	/// Bind this address, with the platform defaults for everything else.
	pub fn new(addr: SocketAddr) -> Self {
		Self {
			addr,
			reuse_port: false,
		}
	}

	/// Share the port with the other sockets bound this way (`SO_REUSEPORT`).
	///
	/// Linux spreads inbound datagrams across every socket in the group, which is
	/// what lets one worker per core own a socket on the same port instead of
	/// funnelling every packet through one. Enable it on *every* member: the first
	/// socket to bind without it owns the port outright and the rest fail.
	///
	/// Linux-only, and [`udp`] fails with [`io::ErrorKind::Unsupported`] elsewhere
	/// rather than binding a group the platform does not balance. macOS and the
	/// BSDs accept the option and then deliver a unicast flow to a single member,
	/// so the workers would come up looking healthy with one of them serving
	/// everything.
	pub fn with_reuse_port(mut self, enabled: bool) -> Self {
		self.reuse_port = enabled;
		self
	}
}

/// Bind a UDP socket, making an IPv6 socket dual-stack so it also serves IPv4.
///
/// The socket buffers are grown to 8 MiB where the OS allows it, and a warning
/// names the sysctl to raise where it doesn't.
pub fn udp(options: Udp) -> io::Result<UdpSocket> {
	let addr = options.addr;

	let domain = if addr.is_ipv4() { Domain::IPV4 } else { Domain::IPV6 };
	let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
	make_dual_stack(&socket, addr);
	grow_buffers(&socket);
	if options.reuse_port {
		set_reuse_port(&socket)?;
	}
	socket.bind(&addr.into())?;
	Ok(socket.into())
}

/// Request [`UDP_BUFFER`] in both directions, best-effort.
fn grow_buffers(socket: &Socket) {
	for direction in [Direction::Recv, Direction::Send] {
		direction.grow(socket);
	}
}

/// One direction of a socket's buffering, owning everything that differs between
/// the two: the socket options, the sysctl to name, and the warning.
#[derive(Clone, Copy)]
enum Direction {
	Recv,
	Send,
}

impl Direction {
	/// Raise this direction's buffer to [`UDP_BUFFER`], then read back what the
	/// kernel actually granted and warn once when it fell short.
	///
	/// Reading back is the whole point: Linux silently clamps the request to its
	/// sysctl, so `setsockopt` returning `Ok` says nothing about the size we ended
	/// up with, and `SO_RCVBUFFORCE` (the clamp-free version) needs `CAP_NET_ADMIN`
	/// that a relay shouldn't be asking for.
	fn grow(self, socket: &Socket) {
		// Never shrink a system that's already tuned above our default.
		if self.size(socket).is_ok_and(sufficient) {
			return;
		}

		match self.set_size(socket, UDP_BUFFER).and_then(|()| self.size(socket)) {
			Ok(reported) if sufficient(reported) => {}
			Ok(reported) => self.warn_short(granted(reported)),
			Err(err) => self.warn_failed(&err),
		}
	}

	fn size(self, socket: &Socket) -> std::io::Result<usize> {
		match self {
			Self::Recv => socket.recv_buffer_size(),
			Self::Send => socket.send_buffer_size(),
		}
	}

	fn set_size(self, socket: &Socket, size: usize) -> std::io::Result<()> {
		match self {
			Self::Recv => socket.set_recv_buffer_size(size),
			Self::Send => socket.set_send_buffer_size(size),
		}
	}

	fn name(self) -> &'static str {
		match self {
			Self::Recv => "receive",
			Self::Send => "send",
		}
	}

	fn sysctl(self) -> Option<&'static str> {
		match self {
			Self::Recv => RECV_SYSCTL,
			Self::Send => SEND_SYSCTL,
		}
	}

	/// One warning per direction per process: a client that reconnects rebinds, and
	/// the operator only needs telling once.
	fn warned(self) -> &'static Once {
		static RECV: Once = Once::new();
		static SEND: Once = Once::new();

		match self {
			Self::Recv => &RECV,
			Self::Send => &SEND,
		}
	}

	/// The kernel accepted the request and quietly handed back `granted` instead.
	fn warn_short(self, granted: usize) {
		self.warned().call_once(|| self.emit_short(granted));
	}

	/// The warning itself, minus the once-guard, so a test can read it back
	/// whatever a previous bind on this host already consumed.
	fn emit_short(self, granted: usize) {
		let name = self.name();
		match self.sysctl() {
			Some(sysctl) => tracing::warn!(
				wanted = UDP_BUFFER,
				granted,
				"UDP {name} buffer is smaller than requested; raise `{sysctl}` or expect packet loss under load"
			),
			None => tracing::warn!(
				wanted = UDP_BUFFER,
				granted,
				"UDP {name} buffer is smaller than requested; expect packet loss under load"
			),
		}
	}

	/// The option itself was rejected, so we don't even know what we're running with.
	fn warn_failed(self, err: &std::io::Error) {
		let name = self.name();
		self.warned()
			.call_once(|| tracing::warn!(%err, "failed to set the UDP {name} buffer size"));
	}
}

/// Whether a buffer size the kernel reported already covers [`UDP_BUFFER`].
fn sufficient(reported: usize) -> bool {
	granted(reported) >= UDP_BUFFER
}

/// The usable size behind a buffer size the kernel reported.
///
/// Linux reports back double what it granted, reserving the other half for
/// per-packet bookkeeping, so halving keeps the numbers we compare and log in the
/// same units an operator writes into the sysctl.
fn granted(reported: usize) -> usize {
	if cfg!(any(target_os = "linux", target_os = "android")) {
		reported / 2
	} else {
		reported
	}
}

/// Set `SO_REUSEPORT`, or report that this platform cannot load-balance a port.
///
/// Not best-effort like the other socket options here: a caller asking for this
/// is building a group of sockets that only works if the kernel spreads traffic
/// over it, so a silent no-op would leave one member serving everything.
#[cfg(target_os = "linux")]
fn set_reuse_port(socket: &Socket) -> io::Result<()> {
	socket.set_reuse_port(true)
}

#[cfg(not(target_os = "linux"))]
fn set_reuse_port(_socket: &Socket) -> io::Result<()> {
	Err(io::Error::new(
		io::ErrorKind::Unsupported,
		"SO_REUSEPORT load balancing is Linux-only",
	))
}

/// Whether `socket` also reaches IPv4, through IPv4-mapped addresses.
///
/// [`udp`] clears `IPV6_V6ONLY` best-effort, so this reads back what the
/// platform actually did rather than assuming it took. A socket that stayed
/// v6-only can't send to a mapped destination, and it looks identical from the
/// outside: `local_addr` reads `[::]` either way. Always false for an IPv4
/// socket, which reaches IPv4 natively rather than through mapping.
#[cfg(any(feature = "noq", feature = "quinn", feature = "quiche"))]
pub(crate) fn udp_is_dual_stack(socket: &UdpSocket) -> bool {
	match socket.local_addr() {
		Ok(addr) if addr.is_ipv6() => socket2::SockRef::from(socket).only_v6().is_ok_and(|only| !only),
		_ => false,
	}
}

/// Bind a TCP listener, making an IPv6 socket dual-stack so it also serves IPv4.
///
/// The returned listener is non-blocking, ready to be adopted by an async runtime
/// (`tokio::net::TcpListener::from_std`, `axum_server::from_tcp`).
pub fn tcp(addr: SocketAddr) -> io::Result<TcpListener> {
	let domain = if addr.is_ipv4() { Domain::IPV4 } else { Domain::IPV6 };
	let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
	make_dual_stack(&socket, addr);
	// Match std's TcpListener, which sets SO_REUSEADDR on Unix (not Windows) so a
	// restarted relay can rebind a port still in TIME_WAIT.
	#[cfg(not(windows))]
	socket.set_reuse_address(true)?;
	// Enable keepalive on the listening socket so every accepted connection
	// inherits it (accept() carries socket options across on Linux, macOS, and
	// Windows). Setting it once here reaches every HTTP/HTTPS/WebSocket connection
	// without the serve loop touching each one. Best-effort: a platform that rejects
	// the option keeps the connection rather than failing.
	let keepalive = TcpKeepalive::new()
		.with_time(KEEPALIVE_IDLE)
		.with_interval(KEEPALIVE_INTERVAL);
	if let Err(err) = socket.set_tcp_keepalive(&keepalive) {
		tracing::warn!(%err, "failed to enable TCP keepalive; dead peers may linger");
	}
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

	/// Skip a test when the host has no IPv6 stack (some CI sandboxes and
	/// containers). Creating or binding an IPv6 socket then fails with an
	/// address-family error, which is an environment limitation rather than a
	/// bug in the dual-stack logic. The dual-stack assertion only has meaning
	/// once a socket exists, so there's nothing to verify when IPv6 is absent.
	fn skip_if_no_ipv6(err: &std::io::Error) -> bool {
		// EAFNOSUPPORT / EADDRNOTAVAIL / EPROTONOSUPPORT on Unix, and the WSA*
		// equivalents on Windows. The matching ErrorKinds round out the rest.
		const NO_IPV6_ERRNOS: &[i32] = &[97, 99, 93, 10047, 10049, 10043];
		let no_ipv6 = matches!(
			err.kind(),
			std::io::ErrorKind::AddrNotAvailable | std::io::ErrorKind::Unsupported
		) || err.raw_os_error().is_some_and(|code| NO_IPV6_ERRNOS.contains(&code));
		if no_ipv6 {
			eprintln!("skipping: host has no IPv6 support ({err})");
		}
		no_ipv6
	}

	#[test]
	fn udp_ipv6_is_dual_stack() {
		// An IPv6 wildcard bind should come back dual-stack so IPv4 traffic
		// reaches it. socket2 lets us read the option back to confirm.
		let socket = match udp(Udp::new("[::]:0".parse().unwrap())) {
			Ok(socket) => socket,
			Err(err) if skip_if_no_ipv6(&err) => return,
			Err(err) => panic!("failed to bind IPv6 UDP socket: {err}"),
		};
		let socket = Socket::from(socket);
		assert!(!socket.only_v6().unwrap(), "IPv6 socket should be dual-stack");
	}

	#[test]
	fn udp_buffers_grow() {
		fn check(direction: Direction) {
			let plain = Socket::from(std::net::UdpSocket::bind("127.0.0.1:0").unwrap());
			let before = direction.size(&plain).unwrap();

			let tuned = Socket::from(udp(Udp::new("127.0.0.1:0".parse().unwrap())).unwrap());
			let after = direction.size(&tuned).unwrap();

			// A host whose default already covers UDP_BUFFER is left alone. Anywhere
			// else the bind has to have actually raised it, whatever the sysctls
			// clamped it to.
			if sufficient(before) {
				assert_eq!(after, before, "{} buffer should be left alone", direction.name());
			} else {
				assert!(after > before, "{} buffer should grow past {before}", direction.name());
			}
		}

		check(Direction::Recv);
		check(Direction::Send);
	}

	#[test]
	fn sufficient_accounts_for_the_doubled_report() {
		// Doubled, since that's how Linux reports back a buffer it granted.
		assert!(sufficient(UDP_BUFFER * 2));
		assert!(!sufficient(512 * 1024));
	}

	#[tracing_test::traced_test]
	#[test]
	fn a_clamped_buffer_warns_and_names_the_sysctl() {
		Direction::Recv.emit_short(512 * 1024);

		assert!(logs_contain("UDP receive buffer is smaller than requested"));
		if let Some(sysctl) = Direction::Recv.sysctl() {
			assert!(logs_contain(sysctl));
		}
	}

	#[test]
	fn udp_ipv4_still_binds() {
		let socket = udp(Udp::new("127.0.0.1:0".parse().unwrap())).unwrap();
		assert!(socket.local_addr().unwrap().is_ipv4());
	}

	/// A reuseport group is only useful if every member can hold the same port,
	/// so bind a second socket to the first one's address and check it takes.
	#[test]
	#[cfg(target_os = "linux")]
	fn udp_reuse_port_shares_a_port() {
		let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
		let first = udp(Udp::new(addr).with_reuse_port(true)).unwrap();
		let bound = first.local_addr().unwrap();
		let second = udp(Udp::new(bound).with_reuse_port(true)).unwrap();
		assert_eq!(second.local_addr().unwrap(), bound);
	}

	/// Without the option the second bind must lose the port, which is what makes
	/// a missed `with_reuse_port` on one member a startup failure rather than a
	/// silently lopsided group.
	#[test]
	#[cfg(target_os = "linux")]
	fn udp_without_reuse_port_keeps_the_port() {
		let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
		let first = udp(Udp::new(addr)).unwrap();
		let bound = first.local_addr().unwrap();
		assert!(udp(Udp::new(bound).with_reuse_port(true)).is_err());
	}

	/// The point of the option is that the kernel spreads traffic over the group,
	/// which is what a worker-per-core listener is built on. Send from a spread of
	/// source ports and check that more than one member is fed.
	#[test]
	#[cfg(target_os = "linux")]
	fn udp_reuse_port_spreads_datagrams() {
		const MEMBERS: usize = 4;
		const SENDERS: usize = 64;

		let mut group = Vec::with_capacity(MEMBERS);
		let mut addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
		for _ in 0..MEMBERS {
			let socket = udp(Udp::new(addr).with_reuse_port(true)).unwrap();
			addr = socket.local_addr().unwrap();
			socket.set_nonblocking(true).unwrap();
			group.push(socket);
		}

		// Each sender gets its own ephemeral source port, which is what the
		// kernel hashes on.
		for _ in 0..SENDERS {
			let sender = udp(Udp::new("127.0.0.1:0".parse().unwrap())).unwrap();
			sender.send_to(b"quic", addr).unwrap();
		}

		let mut fed = 0;
		let mut total = 0;
		for socket in &group {
			let mut received = 0;
			let mut buf = [0u8; 8];
			while socket.recv_from(&mut buf).is_ok() {
				received += 1;
			}
			total += received;
			fed += usize::from(received > 0);
		}

		assert_eq!(total, SENDERS, "every datagram reached exactly one member");
		assert!(fed > 1, "only {fed} of {MEMBERS} members were fed");
	}

	/// Elsewhere the request fails loudly instead of binding a group the kernel
	/// won't balance.
	#[test]
	#[cfg(not(target_os = "linux"))]
	fn udp_reuse_port_is_linux_only() {
		let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
		let err = udp(Udp::new(addr).with_reuse_port(true)).unwrap_err();
		assert_eq!(err.kind(), io::ErrorKind::Unsupported);
	}

	#[test]
	fn tcp_ipv6_is_dual_stack() {
		let listener = match tcp("[::]:0".parse().unwrap()) {
			Ok(listener) => listener,
			Err(err) if skip_if_no_ipv6(&err) => return,
			Err(err) => panic!("failed to bind IPv6 TCP listener: {err}"),
		};
		let socket = Socket::from(listener);
		assert!(!socket.only_v6().unwrap(), "IPv6 listener should be dual-stack");
	}
}
