//! A wildcard-bound socket answers from the local address a peer reached, as a
//! multihomed host needs: a peer that dialed a secondary address drops a reply
//! from the primary one. Covered for IPv4, IPv6 and a dual-stack socket with an
//! IPv4-mapped peer, each reached on a second local address.
//!
//! All of 127/8 is local, so IPv4 reaches `127.0.0.2`. IPv6 has only `::1`, so
//! the IPv6 case is ignored by default and `just rs uring` runs it after its CI
//! lanes add [`SECOND_V6`] to `lo`; without it the case skips loudly, and
//! CI_STRICT makes CI refuse it.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::os::fd::AsRawFd;
use std::time::Duration;

use moq_uring::{Config, Error, Worker, udp};
use nix::sys::socket::{AddressFamily, SockFlag, SockType, SockaddrIn6, bind, setsockopt, socket, sockopt};

const SECOND_V4: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 2);
/// Added by `sudo ip -6 addr add fd6d:6f71::2/128 dev lo nodad` in the io_uring CI lanes.
const SECOND_V6: Ipv6Addr = Ipv6Addr::new(0xfd6d, 0x6f71, 0, 0, 0, 0, 0, 2);
const TIMEOUT: Duration = Duration::from_secs(5);

fn strict() -> bool {
	std::env::var_os("CI_STRICT").is_some()
}

fn worker() -> Option<Worker> {
	match Worker::new(Config::default()) {
		Ok(worker) => Some(worker),
		Err(Error::Unsupported(reason)) if !strict() => {
			eprintln!("skipping io_uring local address test: {reason}");
			None
		}
		Err(err) => panic!("worker setup failed: {err}"),
	}
}

/// Whether [`SECOND_V6`] is on this host, skipping loudly when it is not.
fn second_v6() -> bool {
	match UdpSocket::bind((SECOND_V6, 0)) {
		Ok(_) => true,
		Err(err) if !strict() => {
			eprintln!("skipping IPv6 local address test, {SECOND_V6} is not local: {err}");
			false
		}
		Err(err) => panic!("{SECOND_V6} is not local; add it to lo: {err}"),
	}
}

/// A wildcard `[::]` socket, accepting IPv4 too unless `v6_only`.
fn wildcard_v6(v6_only: bool) -> UdpSocket {
	let fd = socket(AddressFamily::Inet6, SockType::Datagram, SockFlag::empty(), None).expect("socket");
	setsockopt(&fd, sockopt::Ipv6V6Only, &v6_only).expect("IPV6_V6ONLY");
	let any: std::net::SocketAddrV6 = "[::]:0".parse().expect("addr");
	bind(fd.as_raw_fd(), &SockaddrIn6::from(any)).expect("bind");
	UdpSocket::from(fd)
}

fn ablated() -> udp::Config {
	let mut config = udp::Config::default();
	config.gro = false;
	config.gso = false;
	config.multishot = false;
	config
}

/// A peer at `peer` sends to `server` on `reach`, and the server answers from
/// the address the datagram arrived on. Returns that address and the source
/// the peer saw the answer come from.
fn answer(server: UdpSocket, peer: &str, reach: IpAddr, config: udp::Config) -> Option<(Option<IpAddr>, IpAddr)> {
	let mut worker = worker()?;
	let handle = worker.handle();
	let server = handle.udp(server, config).expect("server socket");
	let to = SocketAddr::new(reach, server.local_addr().expect("server addr").port());

	let peer = UdpSocket::bind(peer).expect("bind peer");
	peer.set_read_timeout(Some(TIMEOUT)).expect("timeout");
	let (done_tx, done_rx) = tokio::sync::oneshot::channel();
	let client = std::thread::spawn(move || {
		peer.send_to(b"ping", to).expect("send");
		let mut buf = [0u8; 16];
		let (len, from) = peer.recv_from(&mut buf).expect("no answer");
		assert_eq!(&buf[..len], b"pong");
		done_tx.send(()).expect("notify server");
		from.ip()
	});

	let dst_ip = worker
		.block_on(async move {
			let packet = server.recv().await.expect("recv");
			let mut tx = server.acquire().await.expect("acquire");
			tx.extend_from_slice(b"pong");
			// Marked too, so a send carries every control message at once.
			let mut transmit = udp::Transmit::new(packet.from(), 4);
			transmit.ecn = Some(udp::Ecn::Ect0);
			transmit.src_ip = packet.dst_ip();
			tx.send(transmit).expect("send");
			done_rx.await.expect("peer received the answer");
			packet.dst_ip()
		})
		.expect("worker");
	Some((dst_ip, client.join().expect("client thread")))
}

#[test]
fn a_v4_wildcard_answers_from_the_address_reached() {
	for config in [udp::Config::default(), ablated()] {
		let server = UdpSocket::bind("0.0.0.0:0").expect("bind");
		let Some((dst_ip, from)) = answer(server, "127.0.0.1:0", SECOND_V4.into(), config) else {
			return;
		};
		assert_eq!(dst_ip, Some(SECOND_V4.into()));
		assert_eq!(from, IpAddr::from(SECOND_V4));
	}
}

#[test]
#[ignore = "needs fd6d:6f71::2 on lo; run by just rs uring"]
fn a_v6_wildcard_answers_from_the_address_reached() {
	if !second_v6() {
		return;
	}
	for config in [udp::Config::default(), ablated()] {
		let Some((dst_ip, from)) = answer(wildcard_v6(true), "[::1]:0", SECOND_V6.into(), config) else {
			return;
		};
		assert_eq!(dst_ip, Some(SECOND_V6.into()));
		assert_eq!(from, IpAddr::from(SECOND_V6));
	}
}

/// A dual-stack socket reads an IPv4 datagram's local address v4-mapped, and
/// answering from it leaves as IPv4 from that address.
#[test]
fn a_dual_stack_wildcard_answers_a_v4_peer_from_the_address_reached() {
	for config in [udp::Config::default(), ablated()] {
		let Some((dst_ip, from)) = answer(wildcard_v6(false), "127.0.0.1:0", SECOND_V4.into(), config) else {
			return;
		};
		assert_eq!(dst_ip, Some(SECOND_V4.to_ipv6_mapped().into()));
		assert_eq!(from, IpAddr::from(SECOND_V4));
	}
}

/// End to end: a tokio peer completes a handshake with a wildcard-bound
/// endpoint it reached on a secondary address. moq-quic's client drops packets
/// from any other address, so a reply from the primary one stalls it.
#[cfg(feature = "quic")]
#[test]
fn a_wildcard_endpoint_handshakes_on_a_secondary_address() {
	use moq_uring::quic;

	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let certs = crate::support::certs().expect("certificates");
	let server = quic::server::Config::new(
		quic::Identity::open(&certs.cert, &certs.key)
			.expect("identity")
			.server(&[moq_tokio::quic::web::ALPN], quic::server::ClientAuth::None)
			.expect("tls"),
	);
	let socket = handle
		.udp(UdpSocket::bind("0.0.0.0:0").expect("bind"), udp::Config::default())
		.expect("socket");
	let endpoint =
		quic::Endpoint::new(socket, quic::endpoint::Config::default().with_server(server)).expect("endpoint");
	let addr = SocketAddr::new(SECOND_V4.into(), endpoint.local_addr().port());
	let (done_tx, done_rx) = tokio::sync::oneshot::channel();

	let client = std::thread::spawn(move || {
		// Enabling `ring` beside `aws-lc-rs` leaves rustls no implicit default.
		let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
		let runtime = tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.expect("runtime");
		runtime.block_on(async move {
			let request = moq_tokio::quic::web::proto::ConnectRequest::new(
				url::Url::parse(&format!("https://{addr}/")).expect("url"),
			);
			kio::clock::timeout(TIMEOUT, crate::support::web_connect(request, Default::default()))
				.await
				.expect("handshake stalled")
				.expect("connect");
			done_tx.send(()).expect("notify server");
		})
	});

	// The server answers in the background, so a stalled handshake fails on the
	// peer's timeout rather than parking the server forever.
	handle.spawn(async move {
		let conn = endpoint.accept().await.expect("accept");
		let request = quic::web::Request::accept(conn).await.expect("handshake");
		let session = request.ok().await.expect("respond");
		std::future::pending::<()>().await;
		drop(session);
	});
	let connected = worker.block_on(done_rx).expect("worker");
	client.join().expect("client thread");
	connected.expect("peer connected");
}
