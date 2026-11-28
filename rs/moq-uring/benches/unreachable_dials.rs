//! Failing unroutable dials while other handshakes are in flight on the same socket.
//!
//! A send that finds no route fails the handshakes dialing that destination.
//! Its cost should follow the failed destinations alone, not how many other
//! handshakes the endpoint is carrying, so the per-dial time stays flat across
//! the `handshakes` axis.

#[cfg(target_os = "linux")]
mod linux {
	use std::future::Future;
	use std::net::UdpSocket;
	use std::os::fd::AsRawFd;
	use std::pin::Pin;
	use std::task::Poll;

	use moq_uring::quic;
	use nix::sys::socket::{AddressFamily, SockFlag, SockType, SockaddrIn6, bind, setsockopt, socket, sockopt};

	pub type Dial<'a> = Pin<Box<dyn Future<Output = Result<quic::Connection, quic::Error>> + 'a>>;

	pub fn dial_config() -> quic::client::Config {
		// Nothing ever answers, so nothing is verified.
		let mut tls = moq_tokio::tls::Connect::default();
		tls.insecure = Some(true);
		let mut tls = tls.build().expect("tls");
		tls.alpn_protocols = vec![b"moq-uring-bench".to_vec()];
		quic::client::Config::new(std::sync::Arc::new(tls))
	}

	/// An IPv6-only socket, which Linux refuses to send to a v4-mapped address
	/// with `ENETUNREACH`: an unroutable family without touching the routes.
	pub fn v6_only() -> UdpSocket {
		let fd = socket(AddressFamily::Inet6, SockType::Datagram, SockFlag::empty(), None).expect("socket");
		setsockopt(&fd, sockopt::Ipv6V6Only, &true).expect("IPV6_V6ONLY");
		let local: std::net::SocketAddrV6 = "[::1]:0".parse().expect("addr");
		bind(fd.as_raw_fd(), &SockaddrIn6::from(local)).expect("bind");
		UdpSocket::from(fd)
	}

	/// Poll every dial until it resolves, returning how many failed.
	pub async fn settle(mut dials: Vec<Dial<'_>>) -> usize {
		let mut failed = 0;
		std::future::poll_fn(|cx| {
			dials.retain_mut(|dial| match dial.as_mut().poll(cx) {
				Poll::Ready(result) => {
					failed += usize::from(result.is_err());
					false
				}
				Poll::Pending => true,
			});
			if dials.is_empty() {
				Poll::Ready(())
			} else {
				Poll::Pending
			}
		})
		.await;
		failed
	}
}

#[cfg(target_os = "linux")]
use std::{
	fmt,
	net::{Ipv4Addr, SocketAddr, UdpSocket},
	rc::Rc,
	task::Poll,
};

#[cfg(target_os = "linux")]
use moq_uring::{Config, Worker, quic, udp};
#[cfg(target_os = "linux")]
use swole::bench;

#[cfg(target_os = "linux")]
use linux::{Dial, dial_config, settle, v6_only};

/// Handshakes in flight against unroutable destinations dialed per call.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct Case {
	handshakes: usize,
	destinations: u16,
}

#[cfg(target_os = "linux")]
impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "handshakes-{}/{}", self.handshakes, self.destinations)
	}
}

#[cfg(target_os = "linux")]
fn cases() -> Vec<Case> {
	[1, 256, 4096]
		.into_iter()
		.flat_map(|handshakes| {
			[1, 8, 64].map(|destinations| Case {
				handshakes,
				destinations,
			})
		})
		.collect()
}

/// An endpoint carrying `handshakes` dials into a hole, and the destinations a call fails.
///
/// Fields drop in order: the background dials before their endpoint, the worker last.
#[cfg(target_os = "linux")]
struct Dials {
	_background: Vec<Dial<'static>>,
	unroutable: Vec<SocketAddr>,
	config: Rc<quic::client::Config>,
	endpoint: quic::Endpoint,
	_hole: UdpSocket,
	worker: Worker,
}

#[cfg(target_os = "linux")]
fn dials(case: Case) -> Dials {
	let mut worker = Worker::new(Config::default()).expect("io_uring worker");
	let handle = worker.handle();
	let endpoint = quic::Endpoint::new(
		quic::endpoint::Config::default(),
		handle.udp(v6_only(), udp::Config::default()).expect("socket"),
	)
	.expect("endpoint");

	// Handshakes into a socket nobody reads stay in flight for the run, until the 30s
	// handshake timeout ends them.
	let hole = UdpSocket::bind("[::1]:0").expect("bind");
	let into_hole = hole.local_addr().expect("addr");
	let config = Rc::new(dial_config());
	let mut background: Vec<Dial<'static>> = (0..case.handshakes)
		.map(|_| {
			let (endpoint, config) = (endpoint.clone(), config.clone());
			Box::pin(async move { endpoint.connect(&config, into_hole, "localhost").await }) as Dial<'static>
		})
		.collect();
	worker
		.block_on(std::future::poll_fn(|cx| {
			for dial in &mut background {
				assert!(dial.as_mut().poll(cx).is_pending(), "a dial into a hole resolved");
			}
			Poll::Ready(())
		}))
		.expect("worker");

	let unroutable = (1..=case.destinations)
		.map(|port| SocketAddr::new(Ipv4Addr::LOCALHOST.to_ipv6_mapped().into(), port))
		.collect();
	Dials {
		_background: background,
		unroutable,
		config,
		endpoint,
		_hole: hole,
		worker,
	}
}

#[cfg(target_os = "linux")]
#[bench(args = cases(), setup = dials, items = |case: Case| case.destinations)]
fn unreachable_dials(dials: &mut Dials) {
	let pending = dials
		.unroutable
		.iter()
		.map(|&addr| Box::pin(dials.endpoint.connect(&dials.config, addr, "localhost")) as Dial<'_>)
		.collect();
	let failed = dials.worker.block_on(settle(pending)).expect("worker");
	assert_eq!(failed, dials.unroutable.len(), "every unroutable dial fails");
}

fn main() {
	swole::main();
}
