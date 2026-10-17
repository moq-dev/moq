//! Over native QUIC, incomplete frames hold the receiver's connection credit, and when
//! they fill its window the receiver stops the least urgent stream and keeps going.
//!
//! The server publishes with the stock config. The client dials over the raw driver so
//! its connection window can be far smaller than the stack allows, which keeps the
//! payloads small. Without recovery the high-priority frame never arrives: the window
//! is spent on a low-priority frame that can never complete.

#![cfg(feature = "quic")]

use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use moq_tokio::moq_net::{self, Timestamp, frame, track::Subscription};
use moq_tokio::quic::driver;

const TIMEOUT: Duration = Duration::from_secs(10);

/// The client's connection receive window.
const WINDOW: usize = 256 * 1024;

fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

/// A server publishing broadcast `room`, and a client subscribed to its origin over a
/// connection whose receive window is [`WINDOW`].
struct Setup {
	broadcast: moq_net::broadcast::Producer,
	remote: moq_net::broadcast::Consumer,
	_client: moq_net::Session,
	_endpoint: driver::Endpoint,
}

async fn setup(version: &str) -> Setup {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let version: moq_net::Version = version.parse().expect("version");

	let publisher = moq_tokio::origin::spawn();
	let broadcast = publisher.create_broadcast("room").expect("broadcast");
	broadcast.announce(Default::default()).expect("announce");

	let mut config = moq_tokio::listen::Config::default();
	config.bind = Some("127.0.0.1:0".parse().unwrap());
	config.tls.generate = vec!["localhost".into()];
	config.version = vec![version];
	let mut server = config
		.init(Default::default())
		.expect("init server")
		.listen()
		.await
		.expect("listen");
	let port = server.local_addr().expect("addr").port();
	tokio::spawn(async move {
		let request = server.accept().await.expect("accept");
		let (session, _driver) = request.with_publisher(&publisher).ok().await.expect("session");
		let _ = session.closed().await;
	});

	let mut tls = rustls::ClientConfig::builder()
		.dangerous()
		.with_custom_certificate_verifier(Arc::new(AnyCert))
		.with_no_client_auth();
	tls.alpn_protocols = vec![version.alpn().as_bytes().to_vec()];
	let quic = driver::crypto::rustls::QuicClientConfig::try_from(tls).expect("quic config");
	let mut transport = driver::TransportConfig::default();
	transport.receive_window((WINDOW as u32).into());
	let mut client = driver::ClientConfig::new(Arc::new(quic));
	client.transport_config(Arc::new(transport));

	let endpoint = driver::Endpoint::client("127.0.0.1:0".parse().unwrap()).expect("endpoint");
	let conn = endpoint
		.connect_with(client, ([127, 0, 0, 1], port).into(), "localhost")
		.expect("connect")
		.await
		.expect("handshake");

	let subscriber = moq_tokio::origin::spawn();
	let session = moq_tokio::transport::Session::new(
		moq_tokio::quic::web::Session::raw(conn),
		moq_net::transport::Binding::InBand,
	);
	let (client, driver) = moq_net::Client::new()
		.with_versions(version.into())
		.with_subscriber(subscriber.clone())
		.connect(moq_net::time::now(), session)
		.await
		.expect("client handshake");
	tokio::spawn(moq_net::time::run(driver));

	let consumer = subscriber.consume();
	tokio::time::timeout(TIMEOUT, consumer.routed("room"))
		.await
		.expect("announce timeout")
		.expect("routed");
	let remote = tokio::time::timeout(TIMEOUT, consumer.request_broadcast("room", None))
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves");

	Setup {
		broadcast,
		remote,
		_client: client,
		_endpoint: endpoint,
	}
}

impl Setup {
	/// Subscribe to a new track at `priority`.
	async fn subscribe(&self, name: &str, priority: u8) -> (moq_net::track::Producer, moq_net::track::Subscriber) {
		let track = self.broadcast.create_track(name, timed()).unwrap();
		let sub = tokio::time::timeout(
			TIMEOUT,
			self.remote
				.track(name)
				.unwrap()
				.subscribe(Subscription::default().with_priority(priority)),
		)
		.await
		.expect("subscribe timeout")
		.expect("subscribe");
		(track, sub)
	}
}

/// The next group, which must arrive in time.
async fn next_group(sub: &mut moq_net::track::Subscriber) -> moq_net::group::Consumer {
	tokio::time::timeout(TIMEOUT, sub.recv_group())
		.await
		.expect("group timed out")
		.expect("group")
		.expect("track ended")
}

/// The next frame's payload, which must arrive in time.
async fn next_frame(sub: &mut moq_net::track::Subscriber) -> Bytes {
	let mut group = next_group(sub).await;
	tokio::time::timeout(TIMEOUT, group.read_frame())
		.await
		.expect("frame timed out")
		.expect("frame")
		.expect("group ended")
		.payload
}

/// Two partial frames that together exceed the window: the lower-priority one is
/// stopped, and the higher-priority one completes on the credit it returns.
async fn the_less_urgent_partial_frame_gives_way(version: &str) {
	let setup = setup(version).await;
	let (low, mut low_sub) = setup.subscribe("low", 1).await;
	let (high, mut high_sub) = setup.subscribe("high", 200).await;

	// Three quarters of the window, held by a frame that never finishes.
	let mut low_group = low.append_group().unwrap();
	let mut low_frame = low_group
		.create_frame(frame::Info {
			size: WINDOW as u64,
			timestamp: Some(Timestamp::ZERO),
		})
		.unwrap();
	low_frame.write(vec![1u8; WINDOW * 3 / 4]).unwrap();

	// Wait until the receiver holds all of it, so nothing else got there first.
	let mut partial = next_group(&mut low_sub).await;
	let mut partial = tokio::time::timeout(TIMEOUT, partial.next_frame())
		.await
		.expect("frame header timed out")
		.expect("frame")
		.expect("group ended");
	let mut seen = 0;
	while seen < WINDOW * 3 / 4 {
		let chunk = tokio::time::timeout(TIMEOUT, partial.read_chunk())
			.await
			.expect("partial payload timed out")
			.expect("chunk")
			.expect("frame ended");
		seen += chunk.len();
	}

	// Half the window, which only fits once the low frame lets go.
	let payload = Bytes::from(vec![2u8; WINDOW / 2]);
	let mut high_group = high.append_group().unwrap();
	high_group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
	high_group.finish().unwrap();
	assert_eq!(next_frame(&mut high_sub).await, payload, "{version}");

	// The session survives, and the low track keeps delivering what fits.
	drop(low_frame);
	drop(low_group);
	let mut group = low.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, &b"after"[..]).unwrap();
	group.finish().unwrap();
	assert_eq!(next_frame(&mut low_sub).await, &b"after"[..], "{version}");
}

#[tokio::test]
async fn lite_partial_frame_gives_way() {
	the_less_urgent_partial_frame_gives_way("moq-lite-06").await;
}

#[tokio::test]
async fn ietf_partial_frame_gives_way() {
	the_less_urgent_partial_frame_gives_way("moq-transport-16").await;
}

/// Accepts any server certificate: the test dials its own self-signed server.
#[derive(Debug)]
struct AnyCert;

impl rustls::client::danger::ServerCertVerifier for AnyCert {
	fn verify_server_cert(
		&self,
		_: &rustls::pki_types::CertificateDer<'_>,
		_: &[rustls::pki_types::CertificateDer<'_>],
		_: &rustls::pki_types::ServerName<'_>,
		_: &[u8],
		_: rustls::pki_types::UnixTime,
	) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
		Ok(rustls::client::danger::ServerCertVerified::assertion())
	}

	fn verify_tls12_signature(
		&self,
		_: &[u8],
		_: &rustls::pki_types::CertificateDer<'_>,
		_: &rustls::DigitallySignedStruct,
	) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
		Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
	}

	fn verify_tls13_signature(
		&self,
		_: &[u8],
		_: &rustls::pki_types::CertificateDer<'_>,
		_: &rustls::DigitallySignedStruct,
	) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
		Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
	}

	fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
		rustls::crypto::aws_lc_rs::default_provider()
			.signature_verification_algorithms
			.supported_schemes()
	}
}
