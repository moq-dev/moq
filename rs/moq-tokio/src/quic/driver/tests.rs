#![cfg(any(feature = "aws-lc-rs", feature = "ring"))]

#[cfg(all(feature = "aws-lc-rs", not(feature = "ring")))]
use rustls::crypto::aws_lc_rs::default_provider;
#[cfg(feature = "ring")]
use rustls::crypto::ring::default_provider;

use std::{
	future::{Future, poll_fn},
	io::{self, IoSliceMut},
	net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket},
	num::NonZeroU64,
	pin::pin,
	str,
	sync::{
		Arc, Condvar, Mutex,
		atomic::{AtomicBool, AtomicUsize, Ordering},
	},
	task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};

use crate::quic::driver::runtime::{AsyncUdpSocket, wrap_udp_socket};
use crate::quic::driver::{Duration, Instant};
use bytes::{Buf, Bytes};
use kio::clock::{sleep, timeout};
use moq_quic::{
	ConnectionId, RandomConnectionIdGenerator, Version,
	crypto::rustls::{QuicClientConfig, QuicServerConfig},
	crypto::{Keys, ServerConfig as ProtoServerConfig, Session},
	transport_parameters::TransportParameters,
};
use moq_sock::udp::RecvMeta;
use rand::{Rng, SeedableRng, rngs::StdRng};
use rustls::{
	RootCertStore,
	pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
};
use tokio::{
	join,
	runtime::{Builder, Runtime},
};
use tracing::instrument::Instrument as _;
use tracing::{error_span, info};
use tracing_subscriber::EnvFilter;

use super::{
	VarInt, client,
	connection::Connection,
	endpoint::{self, Endpoint},
	recv, send, server, transport,
};

#[test]
fn handshake_timeout() {
	let _guard = subscribe();
	let runtime = rt_threaded();
	let client = {
		let _guard = runtime.enter();
		client_endpoint(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap()
	};

	// Avoid NoRootAnchors error
	let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
	let mut roots = RootCertStore::empty();
	roots.add(cert.cert.into()).unwrap();

	let mut client_config = client::Config::with_root_certificates(Arc::new(roots)).unwrap();
	const IDLE_TIMEOUT: Duration = Duration::from_millis(500);
	let mut transport_config = transport::Config::default();
	transport_config.idle_timeout = Some(IDLE_TIMEOUT);
	transport_config.handshake_timeout = IDLE_TIMEOUT;
	transport_config.initial_rtt = Duration::from_millis(10);
	client_config.transport = transport_config;

	let start = Instant::now();
	runtime.block_on(async move {
		match client
			.connect(
				&client_config,
				SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1),
				"localhost",
			)
			.unwrap()
			.await
		{
			Err(crate::quic::driver::connection::Error::HandshakeTimeout) => {}
			Err(e) => panic!("unexpected error: {e:?}"),
			Ok(_) => panic!("unexpected success"),
		}
	});
	let dt = start.elapsed();
	assert!(dt > IDLE_TIMEOUT && dt < 2 * IDLE_TIMEOUT);
}

#[tokio::test]
async fn close_endpoint() {
	let _guard = subscribe();

	// Avoid NoRootAnchors error
	let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
	let mut roots = RootCertStore::empty();
	roots.add(cert.cert.into()).unwrap();

	let endpoint = client_endpoint(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap();
	endpoint.set_default_client_config(client::Config::with_root_certificates(Arc::new(roots)).unwrap());

	let conn = endpoint
		.dial(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1234), "localhost")
		.unwrap();

	tokio::spawn(async move {
		let _ = conn.await;
	});

	let conn = endpoint
		.dial(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1234), "localhost")
		.unwrap();
	drop(endpoint.close(0u32.into(), ""));
	match conn.await {
		Err(crate::quic::driver::connection::Error::LocallyClosed { .. }) => (),
		Err(e) => panic!("unexpected error: {e}"),
		Ok(_) => {
			panic!("unexpected success");
		}
	}
}

#[test]
fn local_addr() {
	let socket = UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).unwrap();
	let addr = socket.local_addr().unwrap();
	let runtime = rt_basic();
	let ep = {
		let _guard = runtime.enter();
		Endpoint::new(endpoint::Config::default(), socket).unwrap()
	};
	assert_eq!(addr, ep.local_addr());
}

#[test]
fn read_after_close() {
	let _guard = subscribe();
	let runtime = rt_basic();
	let endpoint = {
		let _guard = runtime.enter();
		endpoint()
	};

	const MSG: &[u8] = b"goodbye!";
	let endpoint2 = endpoint.clone();
	runtime.spawn(async move {
		let new_conn = endpoint2
			.accept()
			.await
			.expect("endpoint")
			.accept()
			.expect("accept")
			.await
			.expect("connection");
		let mut s = new_conn.open_uni(Default::default()).await.unwrap();
		s.write_all(MSG).await.unwrap();
		let s = s.finish();
		// Wait for the stream to be closed, one way or another.
		_ = s.stopped().await;
	});
	runtime.block_on(async move {
		let new_conn = endpoint
			.dial(endpoint.local_addr(), "localhost")
			.unwrap()
			.await
			.expect("connect");
		sleep(Duration::from_millis(100)).await;
		let mut stream = new_conn.accept_uni().await.expect("incoming streams");
		let msg = stream.read_to_end(usize::MAX).await.expect("read_to_end");
		assert_eq!(msg, MSG);
	});
}

#[test]
fn export_keying_material() {
	let _guard = subscribe();
	let runtime = rt_basic();
	let endpoint = {
		let _guard = runtime.enter();
		endpoint()
	};

	runtime.block_on(async move {
		let outgoing_conn_fut = tokio::spawn({
			let endpoint = endpoint.clone();
			async move {
				endpoint
					.dial(endpoint.local_addr(), "localhost")
					.unwrap()
					.await
					.expect("connect")
			}
		});
		let incoming_conn_fut = tokio::spawn({
			let endpoint = endpoint.clone();
			async move {
				endpoint
					.accept()
					.await
					.expect("endpoint")
					.accept()
					.expect("accept")
					.await
					.expect("connection")
			}
		});
		let outgoing_conn = outgoing_conn_fut.await.unwrap();
		let incoming_conn = incoming_conn_fut.await.unwrap();
		let mut i_buf = [0u8; 64];
		incoming_conn
			.export_keying_material(&mut i_buf, b"asdf", b"qwer")
			.unwrap();
		let mut o_buf = [0u8; 64];
		outgoing_conn
			.export_keying_material(&mut o_buf, b"asdf", b"qwer")
			.unwrap();
		assert_eq!(&i_buf[..], &o_buf[..]);
	});
}

#[tokio::test]
async fn ip_blocking() {
	let _guard = subscribe();
	let endpoint_factory = EndpointFactory::new();
	let client_1 = endpoint_factory.endpoint();
	let client_1_addr = client_1.local_addr();
	let client_2 = endpoint_factory.endpoint();
	let server = endpoint_factory.endpoint();
	let server_addr = server.local_addr();
	let server_task = tokio::spawn(async move {
		loop {
			let accepting = server.accept().await.unwrap();
			if accepting.remote_address() == client_1_addr {
				accepting.refuse();
			} else if accepting.remote_address_validated() {
				accepting.accept().expect("accept").await.expect("connection");
			} else {
				accepting.retry().unwrap();
			}
		}
	});
	tokio::join!(
		async move {
			let e = client_1
				.dial(server_addr, "localhost")
				.unwrap()
				.await
				.expect_err("server should have blocked this");
			assert!(
				matches!(e, crate::quic::driver::connection::Error::ConnectionClosed(_)),
				"wrong error"
			);
		},
		async move {
			client_2.dial(server_addr, "localhost").unwrap().await.expect("connect");
		}
	);
	server_task.abort();
}

/// Construct an endpoint suitable for connecting to itself
fn endpoint() -> Endpoint {
	EndpointFactory::new().endpoint()
}

/// A dial-only endpoint on `addr`, dual-stack when the platform allows it for an IPv6 wildcard
fn client_endpoint(addr: SocketAddr) -> Result<Endpoint, endpoint::Error> {
	Endpoint::new(endpoint::Config::default(), bind(addr)?)
}

/// An endpoint serving `config` on `addr`, dual-stack as [`client_endpoint`]
fn server_endpoint(config: server::Config, addr: SocketAddr) -> Result<Endpoint, endpoint::Error> {
	Endpoint::new(endpoint::Config::default().with_server(config), bind(addr)?)
}

fn bind(addr: SocketAddr) -> io::Result<UdpSocket> {
	moq_sock::bind::udp(moq_sock::bind::Udp::new(addr))
}

fn endpoint_with_config(transport_config: transport::Config) -> Endpoint {
	EndpointFactory::new().endpoint_with_config(transport_config)
}

/// Constructs endpoints suitable for connecting to themselves and each other
struct EndpointFactory {
	cert: rcgen::CertifiedKey<rcgen::KeyPair>,
	endpoint_config: endpoint::Config,
}

impl EndpointFactory {
	fn new() -> Self {
		Self {
			cert: rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap(),
			endpoint_config: endpoint::Config::default(),
		}
	}

	fn endpoint(&self) -> Endpoint {
		self.endpoint_with_config(transport::Config::default())
	}

	fn endpoint_with_config(&self, transport_config: transport::Config) -> Endpoint {
		let key = PrivateKeyDer::Pkcs8(self.cert.signing_key.serialize_der().into());
		let mut server_config =
			crate::quic::driver::server::Config::with_single_cert(vec![self.cert.cert.der().clone()], key).unwrap();
		server_config.transport = transport_config.clone();

		let mut roots = RootCertStore::empty();
		roots.add(self.cert.cert.der().clone()).unwrap();
		let endpoint = Endpoint::new(
			self.endpoint_config.clone().with_server(server_config),
			UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap(),
		)
		.unwrap();
		let mut client_config = client::Config::with_root_certificates(Arc::new(roots)).unwrap();
		client_config.transport = transport_config;
		endpoint.set_default_client_config(client_config);

		endpoint
	}
}

#[derive(Default)]
struct HandshakeBlocker {
	state: Mutex<HandshakeBlockerState>,
	changed: Condvar,
}

#[derive(Default)]
struct HandshakeBlockerState {
	started: bool,
	released: bool,
}

impl HandshakeBlocker {
	fn block(&self) {
		let mut state = self.state.lock().unwrap();
		state.started = true;
		self.changed.notify_all();
		while !state.released {
			state = self.changed.wait(state).unwrap();
		}
	}

	fn wait_until_started(&self) {
		let state = self.state.lock().unwrap();
		let (state, _) = self
			.changed
			.wait_timeout_while(state, Duration::from_secs(5), |state| !state.started)
			.unwrap();
		assert!(state.started, "timed out waiting for handshake to start");
	}

	fn release(&self) {
		let mut state = self.state.lock().unwrap();
		state.released = true;
		self.changed.notify_all();
	}
}

struct HandshakeReleaseGuard(Arc<HandshakeBlocker>);

impl Drop for HandshakeReleaseGuard {
	fn drop(&mut self) {
		self.0.release();
	}
}

struct BlockingServerConfig {
	inner: Arc<dyn ProtoServerConfig>,
	blocker: Arc<HandshakeBlocker>,
}

impl ProtoServerConfig for BlockingServerConfig {
	fn initial_keys(&self, version: Version, dst_cid: ConnectionId) -> Keys {
		self.inner.initial_keys(version, dst_cid)
	}

	fn retry_tag(&self, version: Version, orig_dst_cid: ConnectionId, packet: &[u8]) -> [u8; 16] {
		self.inner.retry_tag(version, orig_dst_cid, packet)
	}

	fn start_session(self: Arc<Self>, version: Version, params: &TransportParameters) -> Box<dyn Session> {
		self.blocker.block();
		self.inner.clone().start_session(version, params)
	}
}

/// Wraps a socket so that a test can make its next receive fail, which ends the endpoint driver
#[derive(Debug)]
struct FailingSocket {
	inner: Box<dyn AsyncUdpSocket>,
	failure: Arc<SocketFailure>,
}

#[derive(Debug, Default)]
struct SocketFailure {
	armed: AtomicBool,
	recv_waker: Mutex<Option<Waker>>,
}

impl SocketFailure {
	/// Make the next receive fail, waking the endpoint driver so that it observes the failure
	fn trigger(&self) {
		self.armed.store(true, Ordering::SeqCst);
		if let Some(waker) = self.recv_waker.lock().unwrap().as_ref() {
			waker.wake_by_ref();
		}
	}
}

impl AsyncUdpSocket for FailingSocket {
	fn poll_recv(
		&self,
		cx: &mut Context<'_>,
		bufs: &mut [IoSliceMut<'_>],
		meta: &mut [RecvMeta],
	) -> Poll<io::Result<usize>> {
		if self.failure.armed.load(Ordering::SeqCst) {
			return Poll::Ready(Err(io::Error::other("injected socket failure")));
		}
		*self.failure.recv_waker.lock().unwrap() = Some(cx.waker().clone());
		self.inner.poll_recv(cx, bufs, meta)
	}

	fn try_send(&self, transmit: &moq_sock::udp::Transmit<'_>) -> io::Result<()> {
		self.inner.try_send(transmit)
	}

	fn poll_writable(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		self.inner.poll_writable(cx)
	}

	fn local_addr(&self) -> io::Result<SocketAddr> {
		self.inner.local_addr()
	}

	fn max_receive_segments(&self) -> usize {
		self.inner.max_receive_segments()
	}

	fn may_fragment(&self) -> bool {
		self.inner.may_fragment()
	}
}

fn localhost_socket() -> Box<dyn AsyncUdpSocket> {
	wrap_udp_socket(UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap()).unwrap()
}

fn blocking_server_pair(
	server_alpn: &[u8],
	client_alpn: &[u8],
	server_socket: Box<dyn AsyncUdpSocket>,
) -> (Endpoint, Endpoint, Arc<HandshakeBlocker>) {
	let factory = EndpointFactory::new();
	let key = PrivateKeyDer::Pkcs8(factory.cert.signing_key.serialize_der().into());
	let mut server_crypto = rustls::ServerConfig::builder_with_provider(default_provider().into())
		.with_protocol_versions(&[&rustls::version::TLS13])
		.unwrap()
		.with_no_client_auth()
		.with_single_cert(vec![factory.cert.cert.der().clone()], key)
		.unwrap();
	server_crypto.alpn_protocols = vec![server_alpn.to_vec()];
	let mut server_config =
		crate::quic::driver::server::Config::new(Arc::new(QuicServerConfig::try_from(server_crypto).unwrap()));
	// Loopback answers in microseconds. The default 333 ms initial RTT would hold every
	// connection closed before its handshake in draining for three seconds.
	let mut transport = transport::Config::default();
	transport.initial_rtt = Duration::from_millis(10);
	server_config.transport = transport.clone();
	let blocker = Arc::new(HandshakeBlocker::default());
	server_config.crypto = Arc::new(BlockingServerConfig {
		inner: server_config.crypto.clone(),
		blocker: blocker.clone(),
	});

	let server = Endpoint::new(
		factory.endpoint_config.clone().with_server(server_config),
		server_socket,
	)
	.unwrap();

	let mut roots = RootCertStore::empty();
	roots.add(factory.cert.cert.der().clone()).unwrap();
	let client = client_endpoint(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap();
	// One classical key share keeps the ClientHello in the first Initial, so a handshake failure
	// surfaces from `accept` itself. aws-lc-rs's default post-quantum share spills it into a
	// second packet, which only reaches the connection after it is registered.
	let mut provider = default_provider();
	provider
		.kx_groups
		.retain(|group| group.name() == rustls::NamedGroup::X25519);
	let mut client_crypto = rustls::ClientConfig::builder_with_provider(provider.into())
		.with_protocol_versions(&[&rustls::version::TLS13])
		.unwrap()
		.with_root_certificates(roots)
		.with_no_client_auth();
	client_crypto.alpn_protocols = vec![client_alpn.to_vec()];
	let mut client_config = client::Config::new(Arc::new(QuicClientConfig::try_from(client_crypto).unwrap()));
	client_config.transport = transport;
	client.set_default_client_config(client_config);

	(client, server, blocker)
}

async fn wait_for_blocked_handshake(blocker: Arc<HandshakeBlocker>) {
	tokio::task::spawn_blocking(move || blocker.wait_until_started())
		.await
		.unwrap();
}

async fn accept_with_blocked_start_session(
	server: Endpoint,
) -> Result<crate::quic::driver::connection::Connection, crate::quic::driver::connection::Error> {
	let incoming = server.accept().await.unwrap();
	// The test crypto provider intentionally blocks in `start_session`, so run
	// accept on the blocking pool rather than parking a Tokio worker thread.
	let connecting = tokio::task::spawn_blocking(move || incoming.accept()).await.unwrap()?;
	connecting.await
}

struct BlockedHandshake {
	client: Endpoint,
	server: Endpoint,
	blocker: Arc<HandshakeBlocker>,
	release_guard: HandshakeReleaseGuard,
	accept: tokio::task::JoinHandle<
		Result<crate::quic::driver::connection::Connection, crate::quic::driver::connection::Error>,
	>,
	connect: tokio::task::JoinHandle<
		Result<crate::quic::driver::connection::Connection, crate::quic::driver::connection::Error>,
	>,
}

fn blocked_handshake(server_alpn: &[u8], client_alpn: &[u8]) -> BlockedHandshake {
	blocked_handshake_with_socket(server_alpn, client_alpn, localhost_socket())
}

fn blocked_handshake_with_socket(
	server_alpn: &[u8],
	client_alpn: &[u8],
	server_socket: Box<dyn AsyncUdpSocket>,
) -> BlockedHandshake {
	let (client, server, blocker) = blocking_server_pair(server_alpn, client_alpn, server_socket);
	let server_addr = server.local_addr();

	let accept = tokio::spawn({
		let server = server.clone();
		async move { accept_with_blocked_start_session(server).await }
	});
	let connect = tokio::spawn({
		let client = client.clone();
		async move { client.dial(server_addr, "localhost").unwrap().await }
	});

	BlockedHandshake {
		client,
		server,
		blocker: blocker.clone(),
		release_guard: HandshakeReleaseGuard(blocker),
		accept,
		connect,
	}
}

async fn await_connection(
	task: tokio::task::JoinHandle<
		Result<crate::quic::driver::connection::Connection, crate::quic::driver::connection::Error>,
	>,
) -> Result<crate::quic::driver::connection::Connection, crate::quic::driver::connection::Error> {
	timeout(Duration::from_secs(5), task).await.unwrap().unwrap()
}

async fn assert_wait_idle_pending(endpoint: &Endpoint) {
	assert!(timeout(Duration::from_millis(50), endpoint.wait_idle()).await.is_err());
}

async fn wait_idle(endpoint: &Endpoint) {
	timeout(Duration::from_secs(5), endpoint.wait_idle()).await.unwrap();
}

/// A parked `wait_idle` must not hang when the endpoint driver is dropped.
///
/// `EndpointDriver::drop` clears the connection senders, which is what makes
/// the endpoint idle, but it wakes only `incoming`. A caller already parked on
/// `idle` is therefore never woken, and with the driver gone nothing else can
/// wake it. `EndpointInner::accept` already guards this same hazard for accepts
/// that complete after driver loss.
#[test]
fn wait_idle_returns_when_the_driver_is_dropped() {
	let _guard = subscribe();
	let factory = EndpointFactory::new();

	// The endpoint under test owns its runtime, so shutting that runtime down
	// drops its driver.
	let endpoint_runtime = rt_threaded();
	let endpoint_handle = endpoint_runtime.handle().clone();
	let endpoint = {
		let _guard = endpoint_runtime.enter();
		factory.endpoint()
	};
	let endpoint_addr = endpoint.local_addr();

	// A peer on a second runtime, so a live connection exists to wait for.
	let peer_runtime = rt_threaded();
	let peer = {
		let _guard = peer_runtime.enter();
		factory.endpoint()
	};
	let peer_handle = peer_runtime.handle().clone();
	// Accept concurrently: the client's handshake cannot complete until the
	// server drives its half, and the accepted handle must stay alive or the
	// connection would close and the endpoint become idle before the assertion.
	let accepted = {
		let handle = endpoint_handle.clone();
		let endpoint = endpoint.clone();
		std::thread::spawn(move || {
			handle.block_on(async {
				let incoming = endpoint.accept().await.expect("incoming");
				incoming.accept().expect("accept").await.expect("accept")
			})
		})
	};
	let connection = peer_handle.block_on(async {
		peer.dial(endpoint_addr, "localhost")
			.expect("connect")
			.await
			.expect("connection")
	});
	let _server_connection = accepted.join().expect("accept thread");

	// Park `wait_idle`; it blocks while that connection is live.
	let (done_tx, done_rx) = std::sync::mpsc::channel();
	let waiter = {
		let handle = endpoint_handle.clone();
		let endpoint = endpoint.clone();
		std::thread::spawn(move || {
			handle.block_on(endpoint.wait_idle());
			let _ = done_tx.send(());
		})
	};

	// Otherwise a pass below would be vacuous.
	assert!(
		done_rx.recv_timeout(Duration::from_millis(200)).is_err(),
		"wait_idle returned before the driver was dropped"
	);

	// Dropping the runtime drops the endpoint driver.
	endpoint_runtime.shutdown_background();

	let woke = done_rx.recv_timeout(Duration::from_secs(10)).is_ok();
	drop(connection);
	drop(peer);
	drop(peer_runtime);
	assert!(woke, "wait_idle never returned after the endpoint driver was dropped");
	let _ = waiter.join();
}

async fn wait_closed(conn: &crate::quic::driver::connection::Connection) -> crate::quic::driver::connection::Error {
	timeout(Duration::from_secs(5), conn.closed()).await.unwrap()
}

#[test]
#[cfg_attr(
	any(target_os = "solaris", target_os = "illumos"),
	ignore = "Fails on Solaris and Illumos"
)]
fn echo_v6() {
	run_echo(EchoArgs {
		client_addr: SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
		server_addr: SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0),
		nr_streams: 1,
		stream_size: 10 * 1024,
		receive_window: None,
		stream_receive_window: None,
	});
}

/// Windows rejects the IPv4 options the driver sets on a dual-stack socket, so an endpoint on a
/// specific IPv6 address must be bound v6-only.
#[tokio::test]
#[cfg_attr(
	any(target_os = "solaris", target_os = "illumos"),
	ignore = "Fails on Solaris and Illumos"
)]
async fn specific_v6_addr() {
	let _guard = subscribe();
	let addr = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0);

	let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
	let key = PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
	let cert = CertificateDer::from(cert.cert);
	let server_config = server::Config::with_single_cert(vec![cert.clone()], key.into()).unwrap();
	let server = server_endpoint(server_config, addr).unwrap();

	let mut roots = RootCertStore::empty();
	roots.add(cert).unwrap();
	let client = client_endpoint(addr).unwrap();
	client.set_default_client_config(client::Config::with_root_certificates(Arc::new(roots)).unwrap());

	tokio::join!(
		async { server.accept().await.unwrap().accept().unwrap().await.unwrap() },
		async { client.dial(server.local_addr(), "localhost").unwrap().await.unwrap() },
	);
}

#[test]
#[cfg_attr(target_os = "solaris", ignore = "Sometimes hangs in poll() on Solaris")]
fn echo_v4() {
	run_echo(EchoArgs {
		client_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
		server_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
		nr_streams: 1,
		stream_size: 10 * 1024,
		receive_window: None,
		stream_receive_window: None,
	});
}

#[test]
#[cfg_attr(target_os = "solaris", ignore = "Hangs in poll() on Solaris")]
fn echo_dualstack() {
	run_echo(EchoArgs {
		client_addr: SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
		server_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
		nr_streams: 1,
		stream_size: 10 * 1024,
		receive_window: None,
		stream_receive_window: None,
	});
}

#[test]
#[ignore = "receive-window stress test; run nightly with just rs quic-stress"]
#[cfg_attr(target_os = "solaris", ignore = "Hangs in poll() on Solaris")]
fn stress_receive_window() {
	run_echo(EchoArgs {
		client_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
		server_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
		nr_streams: 50,
		stream_size: 25 * 1024 + 11,
		receive_window: Some(37),
		stream_receive_window: Some(100 * 1024 * 1024),
	});
}

#[test]
#[ignore = "receive-window stress test; run nightly with just rs quic-stress"]
#[cfg_attr(target_os = "solaris", ignore = "Hangs in poll() on Solaris")]
fn stress_stream_receive_window() {
	// Note that there is no point in running this with too many streams,
	// since the window is only active within a stream.
	run_echo(EchoArgs {
		client_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
		server_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
		nr_streams: 2,
		stream_size: 250 * 1024 + 11,
		receive_window: Some(100 * 1024 * 1024),
		stream_receive_window: Some(37),
	});
}

#[test]
#[ignore = "receive-window stress test; run nightly with just rs quic-stress"]
#[cfg_attr(target_os = "solaris", ignore = "Hangs in poll() on Solaris")]
fn stress_both_windows() {
	run_echo(EchoArgs {
		client_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
		server_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
		nr_streams: 50,
		stream_size: 25 * 1024 + 11,
		receive_window: Some(37),
		stream_receive_window: Some(37),
	});
}

fn run_echo(args: EchoArgs) {
	let _guard = subscribe();
	let runtime = rt_basic();
	let handle = {
		// Use small receive windows
		let mut transport_config = transport::Config::default();
		if let Some(receive_window) = args.receive_window {
			transport_config.limits.receive_window = NonZeroU64::new(receive_window);
		}
		if let Some(stream_receive_window) = args.stream_receive_window {
			transport_config.limits.stream_receive_window = NonZeroU64::new(stream_receive_window);
		}
		transport_config.limits.max_streams = NonZeroU64::new(1);

		// We don't use the `endpoint` helper here because we want two different endpoints with
		// different addresses.
		let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
		let key = PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
		let cert = CertificateDer::from(cert.cert);
		let mut server_config =
			crate::quic::driver::server::Config::with_single_cert(vec![cert.clone()], key.into()).unwrap();

		server_config.transport = transport_config.clone();
		let server_sock = UdpSocket::bind(args.server_addr).unwrap();
		let server_addr = server_sock.local_addr().unwrap();
		let server = {
			let _guard = runtime.enter();
			let _guard = error_span!("server").entered();
			Endpoint::new(endpoint::Config::default().with_server(server_config), server_sock).unwrap()
		};

		let mut roots = RootCertStore::empty();
		roots.add(cert).unwrap();
		let mut client_crypto = rustls::ClientConfig::builder_with_provider(default_provider().into())
			.with_safe_default_protocol_versions()
			.unwrap()
			.with_root_certificates(roots)
			.with_no_client_auth();
		client_crypto.key_log = Arc::new(rustls::KeyLogFile::new());

		let client = {
			let _guard = runtime.enter();
			let _guard = error_span!("client").entered();
			client_endpoint(args.client_addr).unwrap()
		};
		let mut client_config = client::Config::new(Arc::new(QuicClientConfig::try_from(client_crypto).unwrap()));
		client_config.transport = transport_config;
		client.set_default_client_config(client_config);

		let handle = runtime.spawn(async move {
			let incoming = server.accept().await.unwrap();

			// Note for anyone modifying the platform support in this test:
			// If `local_ip` gets available on additional platforms - which
			// requires modifying this test - please update the list of supported
			// platforms in the doc comment of `moq_sock::udp::RecvMeta::dst_ip`.
			if cfg!(target_os = "linux")
				|| cfg!(target_os = "android")
				|| cfg!(target_os = "freebsd")
				|| cfg!(target_os = "openbsd")
				|| cfg!(target_os = "netbsd")
				|| cfg!(target_os = "macos")
				|| cfg!(target_os = "windows")
			{
				let local_ip = incoming.local_ip().expect("Local IP must be available");
				assert!(local_ip.is_loopback());
			} else {
				assert_eq!(None, incoming.local_ip());
			}

			let new_conn = incoming.accept().expect("accept").await.unwrap();
			tokio::spawn(async move {
				while let Ok(stream) = new_conn.accept_bi().await {
					tokio::spawn(echo(stream));
				}
			});
			server.wait_idle().await;
		});

		info!("connecting from {} to {}", args.client_addr, server_addr);
		runtime.block_on(
			async move {
				let new_conn = client.dial(server_addr, "localhost").unwrap().await.expect("connect");

				/// This is just an arbitrary number to generate deterministic test data
				const SEED: u64 = 0x12345678;

				for i in 0..args.nr_streams {
					println!("Opening stream {i}");
					let (mut send, mut recv) = new_conn.open_bi(Default::default()).await.expect("stream open");
					let msg = gen_data(args.stream_size, SEED);

					let send_task = async {
						send.write_all(&msg[..]).await.expect("write");
						send.finish();
					};
					let recv_task = async { recv.read_to_end(usize::MAX).await.expect("read") };

					let (_, data) = tokio::join!(send_task, recv_task);

					assert_eq!(data[..], msg[..], "Data mismatch");
				}
				new_conn.close(0u32.into(), "done");
				client.wait_idle().await;
			}
			.instrument(error_span!("client")),
		);
		handle
	};
	runtime.block_on(handle).unwrap();
}

struct EchoArgs {
	client_addr: SocketAddr,
	server_addr: SocketAddr,
	nr_streams: usize,
	stream_size: usize,
	receive_window: Option<u64>,
	stream_receive_window: Option<u64>,
}

async fn echo((mut send, mut recv): (send::Stream, recv::Stream)) {
	while let Some(chunk) = recv.read_chunk(32 << 10).await.expect("read chunk") {
		send.write_all(chunk).await.expect("write chunk");
	}

	let _ = send.finish();
}

fn gen_data(size: usize, seed: u64) -> Vec<u8> {
	let mut rng: StdRng = SeedableRng::seed_from_u64(seed);
	let mut buf = vec![0; size];
	rng.fill_bytes(&mut buf);
	buf
}

fn subscribe() -> tracing::subscriber::DefaultGuard {
	let sub = tracing_subscriber::FmtSubscriber::builder()
		.with_env_filter(EnvFilter::from_default_env())
		.with_writer(|| TestWriter)
		.finish();
	tracing::subscriber::set_default(sub)
}

struct TestWriter;

impl io::Write for TestWriter {
	fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
		print!("{}", str::from_utf8(buf).expect("tried to log invalid UTF-8"));
		Ok(buf.len())
	}
	fn flush(&mut self) -> io::Result<()> {
		io::stdout().flush()
	}
}

fn rt_basic() -> Runtime {
	Builder::new_current_thread().enable_all().build().unwrap()
}

fn rt_threaded() -> Runtime {
	Builder::new_multi_thread().enable_all().build().unwrap()
}

#[tokio::test]
async fn retry_after_rebind() {
	let _guard = subscribe();
	let factory = EndpointFactory::new();
	let server = factory.endpoint();
	let client = factory.endpoint();
	server
		.rebind(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap())
		.unwrap();

	timeout(Duration::from_secs(5), async {
		let connecting = client.dial(server.local_addr(), "localhost").unwrap();
		let incoming = server.accept().await.unwrap();
		assert!(!incoming.remote_address_validated());
		incoming.retry().unwrap();

		let incoming = server.accept().await.unwrap();
		assert!(incoming.remote_address_validated());
		let (accepted, connected) = join!(incoming.accept().unwrap(), connecting);
		let _server_connection = accepted.unwrap();
		let _client_connection = connected.unwrap();
		drop(server.close(0u32.into(), "done"));
		drop(client.close(0u32.into(), "done"));
	})
	.await
	.expect("retried handshake must complete using the rebound socket");
}

#[tokio::test]
async fn rebind_recv() {
	let _guard = subscribe();

	let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
	let key = PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
	let cert = CertificateDer::from(cert.cert);

	let mut roots = RootCertStore::empty();
	roots.add(cert.clone()).unwrap();

	let client = client_endpoint(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap();
	let mut client_config = client::Config::with_root_certificates(Arc::new(roots)).unwrap();
	client_config.transport.limits.max_streams = NonZeroU64::new(1);
	client.set_default_client_config(client_config);

	let server_config = crate::quic::driver::server::Config::with_single_cert(vec![cert.clone()], key.into()).unwrap();
	let server = {
		let _guard = tracing::error_span!("server").entered();
		server_endpoint(server_config, SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap()
	};
	let server_addr = server.local_addr();

	const MSG: &[u8; 5] = b"hello";

	let write_send = Arc::new(tokio::sync::Notify::new());
	let write_recv = write_send.clone();
	let connected_send = Arc::new(tokio::sync::Notify::new());
	let connected_recv = connected_send.clone();
	let server = tokio::spawn(async move {
		let connection = server.accept().await.unwrap().accept().unwrap().await.unwrap();
		info!("got conn");
		connected_send.notify_one();
		write_recv.notified().await;
		let mut stream = connection.open_uni(Default::default()).await.unwrap();
		stream.write_all(MSG.as_slice()).await.unwrap();
		let stream = stream.finish();
		// Wait for the stream to be closed, one way or another.
		_ = stream.stopped().await;
	});

	let connection = {
		let _guard = tracing::error_span!("client").entered();
		client.dial(server_addr, "localhost").unwrap().await.unwrap()
	};
	info!("connected");
	connected_recv.notified().await;
	client
		.rebind(UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap())
		.unwrap();
	info!("rebound");
	write_send.notify_one();
	let mut stream = connection.accept_uni().await.unwrap();
	assert_eq!(stream.read_to_end(MSG.len()).await.unwrap(), MSG);
	server.await.unwrap();
}

/// Verify endpoint behavior while a connection is between `start_accept` and `finish_accept`:
/// `open_connections` must be 0, and `wait_idle` must not complete while `pending_accepts > 0`.
/// After the accept completes and the connection is closed, `wait_idle` must resolve.
#[tokio::test]
async fn split_accept_wait_idle_and_open_connections() {
	let _guard = subscribe();
	let BlockedHandshake {
		client,
		server,
		blocker,
		release_guard: _release_guard,
		accept,
		connect,
	} = blocked_handshake(b"test", b"test");

	wait_for_blocked_handshake(blocker.clone()).await;
	assert_eq!(server.open_connections(), 0);
	assert_wait_idle_pending(&server).await;

	blocker.release();

	let client_conn = await_connection(connect).await.unwrap();
	let server_conn = await_connection(accept).await.unwrap();
	assert_eq!(server.open_connections(), 1);

	client_conn.close(0u32.into(), "done");
	let _ = wait_closed(&client_conn).await;
	let _ = wait_closed(&server_conn).await;
	wait_idle(&server).await;
	wait_idle(&client).await;
}

/// A failed accept never becomes a connection, so no Drained event will wake idle waiters.
#[tokio::test]
async fn split_accept_failure_wakes_idle_waiter() {
	let _guard = subscribe();
	let BlockedHandshake {
		client,
		server,
		blocker,
		release_guard: _release_guard,
		accept,
		connect,
	} = blocked_handshake(b"server", b"client");

	wait_for_blocked_handshake(blocker.clone()).await;
	assert_eq!(server.open_connections(), 0);

	let mut idle = pin!(server.wait_idle());
	let (waker, wake_counter) = new_count_waker();
	assert!(idle.as_mut().poll(&mut Context::from_waker(&waker)).is_pending());
	assert_eq!(wake_counter.wakes(), 0);

	blocker.release();
	let error = await_connection(accept).await.unwrap_err();
	assert!(matches!(
		error,
		crate::quic::driver::connection::Error::TransportError(error)
			if error.code == moq_quic::TransportErrorCode::crypto(0x78)
	));
	// Repolling wait_idle alone would hide a missing notification now that the endpoint is
	// idle. Verify that the waiter registered before the failure was actually woken.
	assert!(wake_counter.wakes() > 0);
	timeout(Duration::from_secs(5), idle).await.unwrap();
	assert_eq!(server.open_connections(), 0);

	assert!(await_connection(connect).await.is_err());
	wait_idle(&client).await;
}

/// Verify that calling `Endpoint::close` while a connection is in the `Accepting` state
/// (between `start_accept` and `finish_accept`) produces `LocallyClosed` on the server side
/// and `ConnectionClosed` on the client side, and that `wait_idle` resolves afterward.
#[tokio::test]
async fn split_accept_close_during_pending_accept() {
	let _guard = subscribe();
	let BlockedHandshake {
		client,
		server,
		blocker,
		release_guard: _release_guard,
		accept,
		connect,
	} = blocked_handshake(b"test", b"test");

	wait_for_blocked_handshake(blocker.clone()).await;
	drop(server.close(0u32.into(), "closing"));
	assert_wait_idle_pending(&server).await;

	blocker.release();

	match await_connection(accept).await {
		Ok(conn) => {
			let err = wait_closed(&conn).await;
			assert!(matches!(
				err,
				crate::quic::driver::connection::Error::LocallyClosed { .. }
			));
		}
		Err(err) => assert!(matches!(
			err,
			crate::quic::driver::connection::Error::LocallyClosed { .. }
		)),
	}

	match await_connection(connect).await {
		Ok(conn) => {
			let err = wait_closed(&conn).await;
			assert!(matches!(
				err,
				crate::quic::driver::connection::Error::ConnectionClosed(_)
			));
		}
		Err(err) => {
			assert!(matches!(
				err,
				crate::quic::driver::connection::Error::ConnectionClosed(_)
			));
		}
	}

	wait_idle(&server).await;
	wait_idle(&client).await;
	assert_eq!(server.open_connections(), 0);
}

/// An accept still in progress when the endpoint closes will send a close of its own, so
/// `wait_all_draining` must not report the endpoint drained until that accept finishes.
#[tokio::test]
async fn split_accept_close_holds_off_all_draining() {
	let _guard = subscribe();
	let BlockedHandshake {
		client,
		server,
		blocker,
		release_guard: _release_guard,
		accept,
		connect,
	} = blocked_handshake(b"test", b"test");

	wait_for_blocked_handshake(blocker.clone()).await;
	drop(server.close(0u32.into(), "closing"));

	let mut draining = pin!(server.wait_all_draining());
	let (waker, _) = new_count_waker();
	assert!(draining.as_mut().poll(&mut Context::from_waker(&waker)).is_pending());

	blocker.release();
	// The waiter parked above resolves only if the accept's close is accounted for and announced.
	timeout(Duration::from_secs(5), draining).await.unwrap();
	assert!(await_connection(connect).await.is_err());

	let _ = await_connection(accept).await;
	wait_idle(&server).await;
	wait_idle(&client).await;
}

/// `close` refuses attempts still queued for `accept`, so the peer hears back before
/// `wait_all_draining` lets the caller release the socket.
#[tokio::test]
async fn close_refuses_queued_incoming() {
	let _guard = subscribe();
	let factory = EndpointFactory::new();
	let server = factory.endpoint();
	let client = factory.endpoint();

	let connect = client.dial(server.local_addr(), "localhost").unwrap();
	timeout(Duration::from_secs(5), server.wait_queued()).await.unwrap();

	timeout(Duration::from_secs(5), server.close(0u32.into(), "closing"))
		.await
		.unwrap();
	assert!(server.accept().await.is_none());

	// Without the refusal, the attempt sits in the queue and the client waits out its timeout.
	let error = timeout(Duration::from_secs(5), connect).await.unwrap().unwrap_err();
	assert!(matches!(
		error,
		crate::quic::driver::connection::Error::ConnectionClosed(close)
			if close.error_code == moq_quic::TransportErrorCode::CONNECTION_REFUSED
	));
}

/// Taking the last queued attempt and refusing it sends no event, so `accept` itself must wake a
/// parked `wait_all_draining`.
#[tokio::test]
async fn refused_incoming_wakes_all_draining_waiter() {
	let _guard = subscribe();
	let factory = EndpointFactory::new();
	let server = factory.endpoint();
	let client = factory.endpoint();

	let connect = client.dial(server.local_addr(), "localhost").unwrap();
	timeout(Duration::from_secs(5), server.wait_queued()).await.unwrap();

	let mut draining = pin!(server.wait_all_draining());
	let (waker, wake_counter) = new_count_waker();
	assert!(draining.as_mut().poll(&mut Context::from_waker(&waker)).is_pending());
	assert_eq!(wake_counter.wakes(), 0);

	server.accept().await.unwrap().refuse();
	assert!(wake_counter.wakes() > 0);
	timeout(Duration::from_secs(5), draining).await.unwrap();

	assert!(timeout(Duration::from_secs(5), connect).await.unwrap().is_err());
}

/// The endpoint driver may exit while an accept runs without the lock. The connection registered
/// afterwards must still be told that the driver is gone, so that its `Connecting` fails promptly
/// and `wait_idle` does not wait on a connection nobody drives.
#[tokio::test]
async fn split_accept_driver_lost_during_handshake() {
	let _guard = subscribe();
	let failure = Arc::new(SocketFailure::default());
	let socket = Box::new(FailingSocket {
		inner: localhost_socket(),
		failure: failure.clone(),
	});
	let BlockedHandshake {
		client,
		server,
		blocker,
		release_guard: _release_guard,
		accept,
		connect,
	} = blocked_handshake_with_socket(b"test", b"test", socket);

	wait_for_blocked_handshake(blocker.clone()).await;
	// Fail the server socket, which ends the endpoint driver while the handshake is blocked.
	// `accept` yields `None` once the driver is gone.
	failure.trigger();
	assert!(
		timeout(Duration::from_secs(5), server.accept())
			.await
			.unwrap()
			.is_none()
	);

	// Register an idle waiter while the accept is still pending. Nothing but the accept's
	// completion can wake it now that the driver is gone.
	let mut idle = pin!(server.wait_idle());
	let (waker, wake_counter) = new_count_waker();
	assert!(idle.as_mut().poll(&mut Context::from_waker(&waker)).is_pending());
	assert_eq!(wake_counter.wakes(), 0);

	blocker.release();
	// Without the fix this waits for the handshake timeout, well past `await_connection`'s limit.
	assert!(await_connection(accept).await.is_err());
	assert!(wake_counter.wakes() > 0);
	timeout(Duration::from_secs(5), idle).await.unwrap();

	drop(client.close(0u32.into(), "done"));
	assert!(await_connection(connect).await.is_err());
	wait_idle(&client).await;
}

#[tokio::test]
async fn remote_address_follows_migration() {
	let _guard = subscribe();

	let factory = EndpointFactory::new();
	let server = factory.endpoint();
	let server_addr = server.local_addr();
	let client = factory.endpoint();

	let trigger_migration = Arc::new(tokio::sync::Notify::new());
	let trigger_migration_clone = trigger_migration.clone();

	let server_task = tokio::spawn(async move {
		let conn = server.accept().await.unwrap().accept().unwrap().await.unwrap();
		let old_remote = conn.remote_address();

		trigger_migration_clone.notify_one();

		let mut stream = conn.accept_uni().await.unwrap();
		assert_eq!(stream.read_to_end(16).await.unwrap(), b"hello");
		let addr_a = conn.remote_address();
		assert_eq!(addr_a.port(), 23_001);
		assert_ne!(old_remote, addr_a);

		trigger_migration_clone.notify_one();

		for _ in 0..2 {
			let mut stream = conn.accept_uni().await.unwrap();
			assert_eq!(stream.read_to_end(16).await.unwrap(), b"hello");
		}
		let addr_b = conn.remote_address();
		assert_eq!(addr_b.port(), 23_003);
		assert_ne!(addr_a, addr_b);
	});

	let client_conn = client.dial(server_addr, "localhost").unwrap().await.unwrap();

	let mut port = 23_000;
	let mut migrate = async || {
		port += 1;
		client
			.rebind(UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)).unwrap())
			.unwrap();

		// send data from the new socket to trigger migration on the server
		let mut stream = client_conn.open_uni(Default::default()).await.unwrap();
		stream.write_all(&b"hello"[..]).await.unwrap();
		stream.finish();
	};

	trigger_migration.notified().await;
	migrate().await;
	trigger_migration.notified().await;
	migrate().await;
	migrate().await;

	server_task.await.unwrap();
	client_conn.close(0u32.into(), "done");
}

/// A send group from another connection panics where it is passed in, without poisoning the
/// connection's lock: a poisoned lock panics again in every handle's drop, aborting the process.
#[tokio::test]
async fn foreign_send_group_panics_cleanly() {
	use futures::FutureExt;
	use std::panic::{AssertUnwindSafe, catch_unwind};

	let _guard = subscribe();
	let endpoint = endpoint();
	let connect = || async {
		tokio::try_join!(endpoint.dial(endpoint.local_addr(), "localhost").unwrap(), async {
			endpoint.accept().await.unwrap().accept().unwrap().await
		})
		.unwrap()
	};
	let (conn, server_conn) = connect().await;
	let (other, _other_server) = connect().await;
	let group = other.send_group(0);
	let options = super::send::Options {
		group: Some(&group),
		priority: 0,
	};

	let uni = AssertUnwindSafe(conn.open_uni(options)).catch_unwind();
	assert!(uni.await.is_err(), "open_uni accepted a foreign group");
	let bi = AssertUnwindSafe(conn.open_bi(options)).catch_unwind();
	assert!(bi.await.is_err(), "open_bi accepted a foreign group");
	let datagram = catch_unwind(AssertUnwindSafe(|| {
		conn.send_datagram(Bytes::from_static(b"x"), options)
	}));
	assert!(datagram.is_err(), "send_datagram accepted a foreign group");
	let mut send = conn.open_uni(Default::default()).await.unwrap();
	let set = catch_unwind(AssertUnwindSafe(|| send.set_group(Some(&group))));
	assert!(set.is_err(), "set_group accepted a foreign group");

	// The connection still works, and its handles drop without panicking.
	send.write_all(&b"after"[..]).await.unwrap();
	send.finish();
	let mut recv = server_conn.accept_uni().await.unwrap();
	assert_eq!(recv.read_to_end(usize::MAX).await.unwrap(), b"after");
}

#[tokio::test]
async fn stream_id_flow_control() {
	let _guard = subscribe();
	let mut cfg = transport::Config::default();
	cfg.limits.max_streams = NonZeroU64::new(1);
	let endpoint = endpoint_with_config(cfg);

	let (client, server) = tokio::join!(endpoint.dial(endpoint.local_addr(), "localhost").unwrap(), async {
		endpoint.accept().await.unwrap().accept().unwrap().await
	});
	let client = client.unwrap();
	let server = server.unwrap();

	// If `open_uni` doesn't get unblocked when the previous stream is dropped, this will time out.
	tokio::join!(
		async {
			client.open_uni(Default::default()).await.unwrap();
		},
		async {
			client.open_uni(Default::default()).await.unwrap();
		},
		async {
			client.open_uni(Default::default()).await.unwrap();
		},
		async {
			server.accept_uni().await.unwrap();
			server.accept_uni().await.unwrap();
		}
	);
}

#[tokio::test]
async fn two_datagram_readers() {
	let _guard = subscribe();
	let endpoint = endpoint();

	let (client, server) = tokio::join!(endpoint.dial(endpoint.local_addr(), "localhost").unwrap(), async {
		endpoint.accept().await.unwrap().accept().unwrap().await
	});
	let client = client.unwrap();
	let server = server.unwrap();

	let done = tokio::sync::Notify::new();
	let (a, b, ()) = tokio::join!(
		async {
			let x = client.read_datagram().await.unwrap();
			done.notify_waiters();
			x
		},
		async {
			let x = client.read_datagram().await.unwrap();
			done.notify_waiters();
			x
		},
		async {
			server.send_datagram(&b"one"[..], Default::default()).unwrap();
			done.notified().await;
			server.send_datagram(&b"two"[..], Default::default()).unwrap();
		}
	);
	assert!(*a == *b"one" || *b == *b"one");
	assert!(*a == *b"two" || *b == *b"two");
}

#[tokio::test]
async fn multiple_conns_with_zero_length_cids() {
	let _guard = subscribe();
	let mut factory = EndpointFactory::new();
	factory.endpoint_config.cid_generator = Some(Arc::new(|| Box::new(RandomConnectionIdGenerator::new(0))));
	let server = {
		let _guard = error_span!("server").entered();
		factory.endpoint()
	};
	let server_addr = server.local_addr();

	let client1 = {
		let _guard = error_span!("client1").entered();
		factory.endpoint()
	};
	let client2 = {
		let _guard = error_span!("client2").entered();
		factory.endpoint()
	};

	let client1 = async move {
		let conn = client1.dial(server_addr, "localhost").unwrap().await.unwrap();
		conn.closed().await;
	}
	.instrument(error_span!("client1"));
	let client2 = async move {
		let conn = client2.dial(server_addr, "localhost").unwrap().await.unwrap();
		conn.closed().await;
	}
	.instrument(error_span!("client2"));
	let server = async move {
		let client1 = server.accept().await.unwrap().accept().unwrap().await.unwrap();
		let client2 = server.accept().await.unwrap().accept().unwrap().await.unwrap();
		// Both connections are now concurrently live.
		client1.close(42u32.into(), "");
		client2.close(42u32.into(), "");
	}
	.instrument(error_span!("server"));
	tokio::join!(client1, client2, server);
}

#[tokio::test]
async fn stream_stopped() {
	let _guard = subscribe();
	let factory = EndpointFactory::new();
	let server = {
		let _guard = error_span!("server").entered();
		factory.endpoint()
	};
	let server_addr = server.local_addr();

	let client = {
		let _guard = error_span!("client1").entered();
		factory.endpoint()
	};

	let client = async move {
		let conn = client.dial(server_addr, "localhost").unwrap().await.unwrap();
		let mut stream = conn.open_uni(Default::default()).await.unwrap();
		let stopped1 = stream.stopped();
		let stopped2 = stream.stopped();
		let stopped3 = stream.stopped();

		stream.write_all(&b"hi"[..]).await.unwrap();
		// spawn one of the futures into a task
		let stopped1 = tokio::task::spawn(stopped1);
		// verify that both futures resolved
		let (stopped1, stopped2) = tokio::join!(stopped1, stopped2);
		assert!(matches!(stopped1, Ok(Ok(Some(val))) if val == 42u32.into()));
		assert!(matches!(stopped2, Ok(Some(val)) if val == 42u32.into()));
		// drop the stream
		drop(stream);
		// verify that a future also resolves after dropping the stream
		let stopped3 = stopped3.await;
		assert_eq!(stopped3, Ok(Some(42u32.into())));
	};
	let client = timeout(Duration::from_millis(100), client).instrument(error_span!("client"));
	let server = async move {
		let conn = server.accept().await.unwrap().accept().unwrap().await.unwrap();
		let mut stream = conn.accept_uni().await.unwrap();
		let mut buf = [0u8; 2];
		stream.read_exact(&mut buf).await.unwrap();
		stream.stop(42u32.into());
		conn
	}
	.instrument(error_span!("server"));
	let (client, conn) = tokio::join!(client, server);
	client.expect("timeout");
	drop(conn);
}

#[tokio::test]
async fn stream_stopped_2() {
	let _guard = subscribe();
	let endpoint = endpoint();

	let (conn, _server_conn) = tokio::try_join!(endpoint.dial(endpoint.local_addr(), "localhost").unwrap(), async {
		endpoint.accept().await.unwrap().accept().unwrap().await
	})
	.unwrap();
	let send_stream = conn.open_uni(Default::default()).await.unwrap();
	let stopped = timeout(Duration::from_millis(100), send_stream.stopped()).instrument(error_span!("stopped"));
	tokio::pin!(stopped);
	// poll the future once so that the waker is registered.
	tokio::select! {
		biased;
		_x = &mut stopped => {},
		_x = std::future::ready(()) => {}
	}
	// finish and drop the send stream, since dropping it unfinished would reset it
	let send_stream = send_stream.finish();
	drop(send_stream);
	// make sure the stopped future still resolves
	let res = stopped.await;
	assert_eq!(res, Ok(Ok(None)));
}

/// A send stream dropped before `finish` is reset, so the peer can't mistake the truncated data
/// for the whole stream.
#[tokio::test]
async fn dropped_send_stream_resets() {
	let _guard = subscribe();
	let endpoint = endpoint();

	let (conn, server_conn) = tokio::try_join!(endpoint.dial(endpoint.local_addr(), "localhost").unwrap(), async {
		endpoint.accept().await.unwrap().accept().unwrap().await
	})
	.unwrap();
	let mut send_stream = conn.open_uni(Default::default()).await.unwrap();
	send_stream.write_all(&b"truncated"[..]).await.unwrap();
	drop(send_stream);

	let mut recv_stream = server_conn.accept_uni().await.unwrap();
	let res = recv_stream.read_to_end(usize::MAX).await;
	assert!(
		matches!(res, Err(recv::Error::Reset(code)) if code == VarInt::from_u32(0)),
		"{res:?}"
	);
}

/// A send stream finished before it drops still delivers its data and FIN.
#[tokio::test]
async fn finished_send_stream_survives_drop() {
	let _guard = subscribe();
	let endpoint = endpoint();

	let (conn, server_conn) = tokio::try_join!(endpoint.dial(endpoint.local_addr(), "localhost").unwrap(), async {
		endpoint.accept().await.unwrap().accept().unwrap().await
	})
	.unwrap();
	let mut send_stream = conn.open_uni(Default::default()).await.unwrap();
	send_stream.write_all(&b"complete"[..]).await.unwrap();
	let send_stream = send_stream.finish();
	drop(send_stream);

	let mut recv_stream = server_conn.accept_uni().await.unwrap();
	let data = recv_stream.read_to_end(usize::MAX).await.unwrap();
	assert_eq!(data, b"complete");
}

/// Connects `endpoint` to itself, returning the client and server sides
async fn connect_self(endpoint: &Endpoint) -> (Connection, Connection) {
	tokio::try_join!(endpoint.dial(endpoint.local_addr(), "localhost").unwrap(), async {
		endpoint.accept().await.unwrap().accept().unwrap().await
	})
	.unwrap()
}

/// Wait until the peer's STOP_SENDING has been processed
async fn stop_sending_received(conn: &Connection) {
	timeout(Duration::from_secs(10), async {
		while conn.stats().frame_rx.stop_sending == 0 {
			tokio::task::yield_now().await;
		}
	})
	.await
	.expect("no STOP_SENDING arrived");
}

/// A peer's stop code that arrived before a local reset still wins
#[tokio::test]
async fn stream_stop_before_reset_wins() {
	let _guard = subscribe();
	let endpoint = endpoint();
	let (conn, server) = connect_self(&endpoint).await;
	let mut send = conn.open_uni(Default::default()).await.unwrap();
	send.write_all(&b"hello"[..]).await.unwrap();
	let recv = server.accept_uni().await.unwrap();
	recv.stop(5u32.into());
	stop_sending_received(&conn).await;

	let stopped = send.stopped();
	send.reset(7u32.into());
	let stopped = timeout(Duration::from_secs(10), stopped).await;
	assert_eq!(stopped, Ok(Ok(Some(5u32.into()))));
}

/// A STOP_SENDING that arrives after a local reset no longer counts
#[tokio::test]
async fn stream_stop_after_reset_is_ignored() {
	let _guard = subscribe();
	let endpoint = endpoint();
	let (conn, server) = connect_self(&endpoint).await;
	let mut send = conn.open_uni(Default::default()).await.unwrap();
	send.write_all(&b"hello"[..]).await.unwrap();
	let recv = server.accept_uni().await.unwrap();
	// Neither side runs in between, so the peer stops a stream it hasn't seen reset yet.
	let stopped = send.stopped();
	send.reset(7u32.into());
	recv.stop(5u32.into());
	stop_sending_received(&conn).await;

	let stopped = timeout(Duration::from_secs(10), stopped).await;
	assert_eq!(stopped, Ok(Ok(None)));
}

/// A stop code that arrived first still wins once the reset stream is freed before the waiter polls
#[tokio::test]
async fn stream_stop_survives_freed_reset() {
	let _guard = subscribe();
	let endpoint = endpoint();
	let (conn, server) = connect_self(&endpoint).await;
	let mut send = conn.open_uni(Default::default()).await.unwrap();
	send.write_all(&b"hello"[..]).await.unwrap();
	let stopped = send.stopped();
	let recv = server.accept_uni().await.unwrap();
	recv.stop(5u32.into());
	stop_sending_received(&conn).await;

	send.reset(7u32.into());
	timeout(Duration::from_secs(10), async {
		while conn.send_streams() > 0 {
			tokio::task::yield_now().await;
		}
	})
	.await
	.expect("the reset stream was never freed");
	let stopped = timeout(Duration::from_secs(10), stopped).await;
	assert_eq!(stopped, Ok(Ok(Some(5u32.into()))));
}

/// A local reset resolves `stopped`, though no stream event reports it
#[tokio::test]
async fn stream_reset_resolves_stopped() {
	let _guard = subscribe();
	let endpoint = endpoint();
	let (conn, _server) = connect_self(&endpoint).await;
	let mut send = conn.open_uni(Default::default()).await.unwrap();
	send.write_all(&b"hello"[..]).await.unwrap();
	let stopped = send.stopped();
	send.reset(7u32.into());
	let stopped = timeout(Duration::from_secs(10), stopped).await;
	assert_eq!(stopped, Ok(Ok(None)));
}

#[tokio::test]
async fn stream_reset_at_resolves_stopped() {
	let _guard = subscribe();
	let endpoint = endpoint();
	let (conn, _server) = connect_self(&endpoint).await;
	let mut send = conn.open_uni(Default::default()).await.unwrap();
	send.write_all(&b"he"[..]).await.unwrap();
	let offset = send.offset();
	send.write_all(&b"llo"[..]).await.unwrap();
	let stopped = send.stopped();
	send.reset_at(offset, 7u32.into());
	let stopped = timeout(Duration::from_secs(10), stopped).await;
	assert_eq!(stopped, Ok(Ok(None)));
}

/// `stopped` futures made before a reset resolve on it, whether or not they were polled yet
#[tokio::test]
async fn stream_stopped_pending_across_reset() {
	let _guard = subscribe();
	let endpoint = endpoint();
	let (conn, _server) = connect_self(&endpoint).await;
	let mut send = conn.open_uni(Default::default()).await.unwrap();
	send.write_all(&b"hello"[..]).await.unwrap();
	let unpolled = send.stopped();
	let polled = tokio::spawn(send.stopped());
	tokio::task::yield_now().await;
	assert!(!polled.is_finished());
	send.reset(7u32.into());
	let polled = timeout(Duration::from_secs(10), polled).await;
	assert_eq!(polled.expect("polled never resolved").unwrap(), Ok(None));
	let unpolled = timeout(Duration::from_secs(10), unpolled).await;
	assert_eq!(unpolled, Ok(Ok(None)));
}

/// A `stopped` future outliving its stream resolves on the drop reset
#[tokio::test]
async fn dropped_stream_resolves_stopped() {
	let _guard = subscribe();
	let endpoint = endpoint();
	let (conn, _server) = connect_self(&endpoint).await;
	let mut send = conn.open_uni(Default::default()).await.unwrap();
	send.write_all(&b"truncated"[..]).await.unwrap();
	let stopped = send.stopped();
	drop(send);
	let stopped = timeout(Duration::from_secs(10), stopped).await;
	assert_eq!(stopped, Ok(Ok(None)));
	assert_eq!(conn.stopped_waiters(), 0);
}

/// A reset releases the stream's `stopped` waiters, even one dropped while pending
#[tokio::test]
async fn stream_reset_releases_stopped() {
	let _guard = subscribe();
	let endpoint = endpoint();
	let (conn, _server) = connect_self(&endpoint).await;
	let mut send = conn.open_uni(Default::default()).await.unwrap();
	send.write_all(&b"hello"[..]).await.unwrap();
	{
		let mut stopped = pin!(send.stopped());
		let poll = std::future::poll_fn(|cx| Poll::Ready(stopped.as_mut().poll(cx))).await;
		assert!(poll.is_pending());
	}
	assert_eq!(conn.stopped_waiters(), 1);
	send.reset(7u32.into());
	assert_eq!(conn.stopped_waiters(), 0);
}

#[tokio::test]
async fn stream_drop_removes_blocked_reader() {
	let _guard = subscribe();

	for drop_stream in [false, true] {
		let endpoint_factory = EndpointFactory::new();
		let server = endpoint_factory.endpoint();
		let server_address = server.local_addr();
		let client = endpoint_factory.endpoint();

		let server_task = tokio::spawn(async move {
			let conn = server.accept().await.unwrap().accept().unwrap().await.unwrap();
			let mut stream = conn.accept_uni().await.unwrap();

			// read "hello"
			let mut buf = [0u8; 5];
			stream.read_exact(&mut buf).await.unwrap();

			let (waker, wake_counter) = new_count_waker();
			let mut cx = Context::from_waker(&waker);
			// do a blocking read which will add the stream in conn.blocked_readers
			{
				let mut buf = [0u8; 64];
				let mut buf = &mut buf[..];
				let read_fut = stream.read(&mut buf);
				tokio::pin!(read_fut);
				assert!(matches!(read_fut.as_mut().poll(&mut cx), Poll::Pending));
			}

			if !drop_stream {
				assert_eq!(wake_counter.wakes(), 0);
				// We have a blocked reader, closing the connection should wake it. We use this as
				// a proxy to assert that the stream is in conn.blocked_readers.
				conn.close(0u32.into(), "done");
				assert_eq!(wake_counter.wakes(), 1);
			} else {
				// dropping the stream should remove it from conn.blocked_readers, so we don't
				// expect any wakeups
				drop(stream);
				assert_eq!(wake_counter.wakes(), 0, "no wakeups should have occurred");
				conn.close(0u32.into(), "done");
				assert_eq!(wake_counter.wakes(), 0, "no wakeups should have occurred");
			}
		});

		let conn = client.dial(server_address, "localhost").unwrap().await.unwrap();
		let mut stream = conn.open_uni(Default::default()).await.unwrap();
		// need to send some data to actually start the stream
		stream.write_all(&b"hello"[..]).await.unwrap();

		server_task.await.unwrap();
	}
}

/// Test that dropping a `RecvStream` after cancelling a read and then
/// explicitly `stop`ing it doesn't panic.
#[tokio::test]
async fn recv_stream_cancel_stop_drop() {
	let _guard = subscribe();
	let factory = EndpointFactory::new();
	let server = {
		let _guard = error_span!("server").entered();
		factory.endpoint()
	};
	let server_addr = server.local_addr();

	let client = {
		let _guard = error_span!("client").entered();
		factory.endpoint()
	};
	let recv_dropped = tokio::sync::SetOnce::new();
	join!(
		async {
			let conn = server.accept().await.unwrap().accept().unwrap().await.unwrap();
			let mut recv = conn.accept_uni().await.unwrap();
			// Create a future to read from the stream, poll it once, then immediately drop it
			{
				let fut = pin!(recv.read_to_end(usize::MAX));
				let mut cx = Context::from_waker(Waker::noop());
				assert!(fut.poll(&mut cx).is_pending());
			}
			recv_dropped.set(()).unwrap();
			recv.stop(0u32.into());
		},
		async {
			let conn = client.dial(server_addr, "localhost").unwrap().await.unwrap();
			let mut send = conn.open_uni(Default::default()).await.unwrap();
			_ = send.write_all(&b"hello"[..]).await;
			// Don't drop (finish) the send stream until the read has been
			// cancelled by the server, ensuring that read_to_end can't complete
			// immediately.
			recv_dropped.wait().await;
		},
	);
}

#[tokio::test(start_paused = true)]
async fn dropped_endpoint_cleans_up() {
	let _guard = subscribe();

	let endpoint = endpoint();
	let addr = endpoint.local_addr();
	drop(endpoint);
	// Let the driver task run; paused runtimes are guaranteed to drain pending work on sleep.
	sleep(Duration::from_millis(1)).await;
	UdpSocket::bind(addr).expect("the driver released the port");
}

/// Closing the endpoint releases its port even while connection handles outlive it, since every
/// connection sends through the endpoint rather than a socket of its own.
#[tokio::test]
async fn close_releases_the_port_under_live_connections() {
	let _guard = subscribe();

	let endpoint = endpoint();
	let addr = endpoint.local_addr();
	let conns = connect_self(&endpoint).await;
	endpoint.close(0u32.into(), "done").await;
	UdpSocket::bind(addr).expect("close released the port");
	drop(conns);
}

/// Closing right after a rebind releases the old port too, which the endpoint keeps receiving on
/// until traffic arrives on the new socket.
#[tokio::test]
async fn close_releases_the_port_left_by_a_rebind() {
	let _guard = subscribe();

	let endpoint = endpoint();
	let old = endpoint.local_addr();
	endpoint
		.rebind(bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap())
		.unwrap();
	endpoint.close(0u32.into(), "done").await;
	UdpSocket::bind(old).expect("close released the old port");
}

#[tokio::test]
async fn dropped_connection_cleans_up() {
	let _guard = subscribe();
	let endpoint = endpoint();
	join!(
		async {
			endpoint
				.dial(endpoint.local_addr(), "localhost")
				.unwrap()
				.await
				.unwrap()
		},
		async { endpoint.accept().await.unwrap().accept().unwrap().await.unwrap() }
	);
	endpoint.wait_idle().await;
}

/// Leases keep the connection window spent until consumed, while a stream window far smaller
/// than it keeps replenishing; a lease outlives its stream.
#[tokio::test]
async fn leases_spend_the_connection_window() {
	const WINDOW: usize = 32 * 1024;
	let _guard = subscribe();
	let mut transport = transport::Config::default();
	transport.limits.receive_window = NonZeroU64::new(WINDOW as u64);
	transport.limits.stream_receive_window = NonZeroU64::new(4096);
	let endpoint = endpoint_with_config(transport);

	let (client, server) = tokio::try_join!(endpoint.dial(endpoint.local_addr(), "localhost").unwrap(), async {
		endpoint.accept().await.unwrap().accept().unwrap().await
	})
	.unwrap();
	let mut send = client.open_uni(Default::default()).await.unwrap();
	let writer = tokio::spawn(async move {
		send.write_all(&[7; WINDOW + 500][..]).await.unwrap();
		send.finish();
	});

	let mut recv = server.accept_uni().await.unwrap();
	assert_eq!(leased(&mut recv, WINDOW).await, WINDOW);
	let stats = server.stats().credit;
	assert_eq!((stats.leased, stats.available), (WINDOW as u64, 0));

	// Far below the usual update threshold, and still enough to unblock.
	consume(&mut recv, 100).await;
	assert_eq!(leased(&mut recv, WINDOW).await, WINDOW);
	assert_eq!(server.stats().credit.leased, WINDOW as u64);

	consume(&mut recv, WINDOW).await;
	assert_eq!(leased(&mut recv, 400).await, 400);
	let fin = poll_fn(|cx| recv.poll_lease(cx, 401).map_ok(|lease| lease.is_fin())).await;
	assert!(fin.unwrap(), "the FIN ends a wait for more");
	writer.await.unwrap();
	assert_eq!(server.stats().credit.leased, 400, "a lease outlives its stream");
	drop(recv);
	assert_eq!(server.stats().credit.leased, 0);
}

/// Two readers each waiting for more than the window can give both: the lower-priority stream
/// is evicted, and the other gets the window to itself.
#[tokio::test]
async fn stuck_window_evicts_the_lower_priority() {
	const WINDOW: usize = 32 * 1024;
	let _guard = subscribe();
	let mut transport = transport::Config::default();
	transport.limits.receive_window = NonZeroU64::new(WINDOW as u64);
	// Short of the window, so the first stream cannot keep the second from arriving.
	transport.limits.stream_receive_window = NonZeroU64::new(WINDOW as u64 / 4 * 3);
	let endpoint = endpoint_with_config(transport);

	let (client, server) = tokio::try_join!(endpoint.dial(endpoint.local_addr(), "localhost").unwrap(), async {
		endpoint.accept().await.unwrap().accept().unwrap().await
	})
	.unwrap();
	let streams = [
		client.open_uni(Default::default()).await.unwrap(),
		client.open_uni(Default::default()).await.unwrap(),
	];
	let low_id = streams[0].id();
	let writers = streams.map(|mut send| {
		let id = send.id();
		let writer = tokio::spawn(async move {
			// Keep the low stream open: an acknowledged FIN wins over a later STOP_SENDING.
			if send.write_all(&[7; WINDOW][..]).await.is_ok() && id != low_id {
				send.finish().stopped().await.unwrap()
			} else {
				send.stopped().await.unwrap()
			}
		});
		(id, writer)
	});

	let mut low = server.accept_uni().await.unwrap();
	let mut high = server.accept_uni().await.unwrap();
	if low.id() != low_id {
		std::mem::swap(&mut low, &mut high);
	}
	low.set_priority(-1);
	high.set_priority(1);
	let leased = tokio::join!(
		poll_fn(|cx| low.poll_lease(cx, WINDOW).map_ok(|lease| lease.remaining())),
		poll_fn(|cx| high.poll_lease(cx, WINDOW).map_ok(|mut lease| {
			let len = lease.remaining();
			lease.advance(len);
			lease.release();
			len
		})),
	);
	assert_eq!(leased, (Err(recv::Error::Evicted), Ok(WINDOW)));

	// The peer hears of the eviction once the stream is stopped. Drain the survivor to FIN
	// instead of dropping unread data, which would stop that stream too.
	drop(low);
	assert!(
		poll_fn(|cx| high.poll_lease(cx, 1).map_ok(|lease| lease.is_fin()))
			.await
			.unwrap()
	);
	for (id, writer) in writers {
		assert_eq!(writer.await.unwrap(), (id == low_id).then(|| 0u32.into()));
	}
}

/// Wait until `recv` leases at least `want` bytes, returning how many it leased.
async fn leased(recv: &mut recv::Stream, want: usize) -> usize {
	poll_fn(|cx| recv.poll_lease(cx, want).map_ok(|lease| lease.remaining()))
		.await
		.unwrap()
}

/// Consume the first `n` leased bytes of `recv`.
async fn consume(recv: &mut recv::Stream, n: usize) {
	poll_fn(|cx| {
		recv.poll_lease(cx, n).map_ok(|mut lease| {
			lease.advance(n);
			lease.release();
		})
	})
	.await
	.unwrap()
}

/// A lease owns its bytes and holds no lock, so the connection keeps moving its other streams
/// while one is held, and the lease may cross an await in a spawned task.
#[tokio::test]
async fn held_lease_does_not_stall_the_connection() {
	fn assert_send<T: Send>() {}
	assert_send::<recv::Lease<'static>>();

	let _guard = subscribe();
	let endpoint = endpoint();
	let (client, server) = connect_self(&endpoint).await;
	let mut held = client.open_uni(Default::default()).await.unwrap();
	held.write_all(&b"held"[..]).await.unwrap();
	let mut recv = server.accept_uni().await.unwrap();
	let lease = recv.lease(4).await.unwrap();

	let mut other = client.open_uni(Default::default()).await.unwrap();
	other.write_all(&b"other"[..]).await.unwrap();
	other.finish();
	let mut other = server.accept_uni().await.unwrap();
	let read = timeout(Duration::from_secs(10), other.read_to_end(64)).await;
	assert_eq!(read.unwrap().unwrap(), b"other");

	assert_eq!(lease.chunk(), b"held");
	lease.release();
}

/// Once `received_reset` reports a reset, moq-quic has freed the stream, so later reads and
/// leases fail with the same reset instead of finding no stream.
#[tokio::test]
async fn reads_after_a_received_reset_fail_with_it() {
	let _guard = subscribe();
	let endpoint = endpoint();
	let (client, server) = connect_self(&endpoint).await;
	let mut send = client.open_uni(Default::default()).await.unwrap();
	send.write_all(&b"x"[..]).await.unwrap();
	let mut recv = server.accept_uni().await.unwrap();
	send.reset(VarInt::from_u32(7));

	let reset = timeout(Duration::from_secs(10), recv.received_reset()).await.unwrap();
	assert_eq!(reset.unwrap(), Some(VarInt::from_u32(7)));
	let lease = recv.lease(1).await.err();
	assert!(matches!(lease, Some(recv::Error::Reset(code)) if code == VarInt::from_u32(7)));
	let read = recv.read(&mut Vec::new()).await;
	assert!(
		matches!(read, Err(recv::Error::Reset(code)) if code == VarInt::from_u32(7)),
		"{read:?}"
	);
}

/// A tiny datagram, and a tiny stream frame taken from a lease, each reach the application in
/// an allocation of their own rather than as a slice of the receive batch they arrived in, which
/// their credit does not pay for.
#[tokio::test]
async fn tiny_frames_do_not_pin_receive_batches() {
	let _guard = subscribe();
	let endpoint = endpoint();
	let (client, server) = connect_self(&endpoint).await;

	client
		.send_datagram(Bytes::from_static(b"x"), Default::default())
		.unwrap();
	assert!(owns_exactly(server.read_datagram().await.unwrap()));

	let mut send = client.open_uni(Default::default()).await.unwrap();
	send.write_all(&b"y"[..]).await.unwrap();
	let mut recv = server.accept_uni().await.unwrap();
	let taken = poll_fn(|cx| {
		recv.poll_lease(cx, 1).map_ok(|mut lease| {
			let len = lease.chunk().len();
			lease.copy_to_bytes(len)
		})
	})
	.await
	.unwrap();
	assert_eq!(taken, &b"y"[..]);
	assert!(owns_exactly(taken));
}

/// Whether `bytes` is the only handle on an allocation of exactly its own size.
fn owns_exactly(bytes: Bytes) -> bool {
	let len = bytes.len();
	bytes.try_into_mut().is_ok_and(|bytes| bytes.capacity() == len)
}

#[derive(Default)]
struct WakeCounter {
	wakes: AtomicUsize,
}

impl WakeCounter {
	fn wakes(&self) -> usize {
		self.wakes.load(Ordering::SeqCst)
	}
}

fn new_count_waker() -> (Waker, Arc<WakeCounter>) {
	// instance of WakeCounter
	let counter = Arc::new(WakeCounter::default());

	// convert
	let waker = unsafe { Waker::from_raw(raw_waker(counter.clone())) };
	(waker, counter)
}

fn raw_waker(counter: Arc<WakeCounter>) -> RawWaker {
	// Store an Arc<WakeCounter> behind the raw pointer.
	let ptr = Arc::into_raw(counter) as *const ();
	RawWaker::new(ptr, &VTABLE)
}

static VTABLE: RawWakerVTable = RawWakerVTable::new(clone_waker, wake_waker, wake_by_ref_waker, drop_waker);

unsafe fn clone_waker(data: *const ()) -> RawWaker {
	// SAFETY: pointer always comes from `Arc::into_raw()` (see `raw_waker()`)
	let arc = unsafe { Arc::<WakeCounter>::from_raw(data as *const WakeCounter) };
	let cloned = arc.clone();
	std::mem::forget(arc);
	raw_waker(cloned)
}

unsafe fn wake_waker(data: *const ()) {
	// SAFETY: pointer always comes from `Arc::into_raw()` (see `raw_waker()`)
	let arc = unsafe { Arc::<WakeCounter>::from_raw(data as *const WakeCounter) };
	arc.wakes.fetch_add(1, Ordering::SeqCst);
	// arc drops here
}

unsafe fn wake_by_ref_waker(data: *const ()) {
	// SAFETY: pointer always comes from `Arc::into_raw()` (see `raw_waker()`)
	let arc = unsafe { Arc::<WakeCounter>::from_raw(data as *const WakeCounter) };
	arc.wakes.fetch_add(1, Ordering::SeqCst);
	std::mem::forget(arc);
}

unsafe fn drop_waker(data: *const ()) {
	// SAFETY: pointer always comes from `Arc::into_raw()` (see `raw_waker()`)
	drop(unsafe { Arc::<WakeCounter>::from_raw(data as *const WakeCounter) });
}

/// A dial to an address the host cannot route fails at once with `NO_VIABLE_PATH`, rather than at
/// the handshake timeout, so a dial race can move on to the next address.
///
/// Linux refuses a v4-mapped destination on an IPv6-only socket with `ENETUNREACH`, which makes an
/// unroutable family without touching the host's routes.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn unroutable_dial_fails_with_no_viable_path() {
	let _guard = subscribe();

	let socket = socket2::Socket::new(
		socket2::Domain::IPV6,
		socket2::Type::DGRAM,
		Some(socket2::Protocol::UDP),
	)
	.unwrap();
	socket.set_only_v6(true).unwrap();
	socket
		.bind(&SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0).into())
		.unwrap();
	let client = Endpoint::new(endpoint::Config::default(), UdpSocket::from(socket)).unwrap();

	let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
	let mut roots = RootCertStore::empty();
	roots.add(cert.cert.into()).unwrap();
	let client_config = client::Config::with_root_certificates(Arc::new(roots)).unwrap();

	let unroutable = SocketAddr::new(IpAddr::V6(Ipv4Addr::LOCALHOST.to_ipv6_mapped()), 1);
	let err = client
		.connect(&client_config, unroutable, "localhost")
		.unwrap()
		.await
		.expect_err("nothing can be sent to an unroutable address");

	// A swallowed send error waits out the handshake instead and reports `TimedOut`.
	let crate::quic::driver::connection::Error::TransportError(err) = err else {
		panic!("expected a transport error, got {err:?}");
	};
	assert_eq!(err.code, moq_quic::TransportErrorCode::NO_VIABLE_PATH);
}

/// Once established, a connection treats an unreachable destination as loss: a route that drops
/// for a moment, as when a client switches networks, must not end the connection.
#[tokio::test]
async fn unreachable_sends_are_loss_once_established() {
	let _guard = subscribe();

	let factory = EndpointFactory::new();
	let server = factory.endpoint();
	let outage = Arc::new(Outage::default());
	let client = Endpoint::new(
		endpoint::Config::default(),
		Box::new(UnreachableSocket {
			inner: localhost_socket(),
			outage: outage.clone(),
		}) as Box<dyn AsyncUdpSocket>,
	)
	.unwrap();
	let mut roots = RootCertStore::empty();
	roots.add(factory.cert.cert.der().clone()).unwrap();
	client.set_default_client_config(client::Config::with_root_certificates(Arc::new(roots)).unwrap());

	let (client_conn, server_conn) = join!(
		async { client.dial(server.local_addr(), "localhost").unwrap().await },
		async { server.accept().await.unwrap().accept().unwrap().await }
	);
	let client_conn = client_conn.unwrap();
	let server_conn = server_conn.unwrap();

	outage.active.store(true, Ordering::SeqCst);
	let mut send = client_conn.open_uni(Default::default()).await.unwrap();
	send.write_all(&b"hello"[..]).await.unwrap();
	send.finish();
	outage.dropped.notified().await;
	outage.active.store(false, Ordering::SeqCst);

	// The stream arrives on a retransmission once the route is back.
	let received = async {
		let mut recv = server_conn.accept_uni().await.unwrap();
		recv.read_to_end(usize::MAX).await.unwrap()
	};
	tokio::select! {
		received = received => assert_eq!(received, b"hello"),
		reason = client_conn.closed() => panic!("the outage closed the connection: {reason}"),
	}
}

/// A handshake that fails on an unreachable peer drains while the application holds its future.
#[tokio::test(start_paused = true)]
async fn unreachable_handshake_drains_while_held() {
	let _guard = subscribe();

	let factory = EndpointFactory::new();
	let key = PrivateKeyDer::Pkcs8(factory.cert.signing_key.serialize_der().into());
	let server_config =
		crate::quic::driver::server::Config::with_single_cert(vec![factory.cert.cert.der().clone()], key).unwrap();
	let outage = Arc::new(Outage::default());
	outage.active.store(true, Ordering::SeqCst);
	let server = Endpoint::new(
		endpoint::Config::default().with_server(server_config),
		Box::new(UnreachableSocket {
			inner: localhost_socket(),
			outage: outage.clone(),
		}) as Box<dyn AsyncUdpSocket>,
	)
	.unwrap();
	let client = factory.endpoint();

	let connecting = client.dial(server.local_addr(), "localhost").unwrap();
	let incoming = server.accept().await.unwrap();
	let connecting_server = incoming.accept().unwrap();
	// Stop the client retrying, which would only start new handshakes.
	drop(client.close(0u32.into(), "done"));
	drop(connecting);

	let crate::quic::driver::connection::Error::TransportError(err) = connecting_server.await.unwrap_err() else {
		panic!("expected a transport error");
	};
	assert_eq!(err.code, moq_quic::TransportErrorCode::NO_VIABLE_PATH);
	timeout(Duration::from_secs(60), server.wait_idle())
		.await
		.expect("the failed connection never drained out of the endpoint");
}

/// Whether [`UnreachableSocket`] is refusing sends, and a signal once it has refused one.
#[derive(Debug, Default)]
struct Outage {
	active: AtomicBool,
	dropped: tokio::sync::Notify,
}

/// Wraps a socket so that a test can make every send fail as though the peer had no route.
#[derive(Debug)]
struct UnreachableSocket {
	inner: Box<dyn AsyncUdpSocket>,
	outage: Arc<Outage>,
}

impl AsyncUdpSocket for UnreachableSocket {
	fn poll_recv(
		&self,
		cx: &mut Context<'_>,
		bufs: &mut [IoSliceMut<'_>],
		meta: &mut [RecvMeta],
	) -> Poll<io::Result<usize>> {
		self.inner.poll_recv(cx, bufs, meta)
	}

	fn try_send(&self, transmit: &moq_sock::udp::Transmit<'_>) -> io::Result<()> {
		if self.outage.active.load(Ordering::SeqCst) {
			self.outage.dropped.notify_one();
			return Err(io::ErrorKind::NetworkUnreachable.into());
		}
		self.inner.try_send(transmit)
	}

	fn poll_writable(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		self.inner.poll_writable(cx)
	}

	fn local_addr(&self) -> io::Result<SocketAddr> {
		self.inner.local_addr()
	}

	fn max_transmit_segments(&self) -> usize {
		self.inner.max_transmit_segments()
	}
}

/// A handshake the peer never answered has nobody to repeat its close to, so closing the endpoint
/// must not wait out a closing period for it.
#[tokio::test(start_paused = true)]
async fn close_during_handshake_does_not_wait_for_silent_peer() {
	let _guard = subscribe();
	let client = endpoint();
	// Bound but never read, so the handshake stays in flight.
	let silent = UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap();
	let connect = client.dial(silent.local_addr().unwrap(), "localhost").unwrap();
	// Let the driver send the Initial.
	tokio::task::yield_now().await;

	let start = kio::clock::now();
	client.close(0u32.into(), "closing").await;
	let elapsed = start.elapsed();
	assert!(connect.await.is_err());
	assert_eq!(elapsed, Duration::ZERO, "waited {elapsed:?}");
}
