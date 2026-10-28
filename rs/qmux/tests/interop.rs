//! Wire interop with the last release before the stream state machine moved into
//! `moq-quic`, in both roles and on every version both speak.
//!
//! Each scenario runs over an in-memory byte stream on paused time: a transfer large enough
//! to need credit updates each way, a bidirectional echo, a reset code each way, a datagram
//! each way where records carry them, and an application close.

#![cfg(any(feature = "tcp", all(unix, feature = "uds")))]

use bytes::{Bytes, BytesMut};
use moq_net::transport::poll::{RecvStream as _, SendOptions, SendStream as _, Session as _};
use qmux::{Protocol, Session, Version};
use web_transport_trait::{RecvStream as _, SendStream as _, Session as _};

const PROTOCOL: &str = "moq-lite-04";
const LEN: usize = 1 << 20;

fn payload(seed: usize) -> Bytes {
	(0..LEN).map(|i| ((i * 31 + seed) % 251) as u8).collect()
}

fn old_version(version: Version) -> qmux06::Version {
	match version {
		Version::QMux00 => qmux06::Version::QMux00,
		Version::QMux01 => qmux06::Version::QMux01,
		Version::QMux02 => qmux06::Version::QMux02,
		_ => unreachable!(),
	}
}

async fn ours_and_theirs(version: Version, ours_is_client: bool) -> (Session, qmux06::Session) {
	let (a, b) = tokio::io::duplex(64 * 1024);
	let mut ours = qmux::Config::new(version);
	ours.protocol = Protocol::Negotiate(vec![PROTOCOL.into()]);
	let mut theirs = qmux06::Config::new(old_version(version));
	theirs.protocol = qmux06::Protocol::Negotiate(vec![PROTOCOL.into()]);

	let ours_transport = qmux::transport::Stream::new(a);
	let theirs_transport = qmux06::transport::Stream::new(b, theirs.version, theirs.max_record_size);
	if ours_is_client {
		let (ours, theirs) = tokio::join!(
			Session::connect(ours_transport, ours),
			qmux06::Session::accept(theirs_transport, theirs)
		);
		(ours.unwrap(), theirs.unwrap())
	} else {
		let (ours, theirs) = tokio::join!(
			Session::accept(ours_transport, ours),
			qmux06::Session::connect(theirs_transport, theirs)
		);
		(ours.unwrap(), theirs.unwrap())
	}
}

async fn write_all(send: &mut qmux::SendStream, mut data: Bytes) {
	while !data.is_empty() {
		send.write_buf(&mut data).await.unwrap();
	}
}

async fn read_all(recv: &mut qmux::RecvStream) -> Result<Bytes, qmux::Error> {
	let mut buf = BytesMut::new();
	while let Some(chunk) = recv.read_chunk(usize::MAX).await? {
		buf.extend_from_slice(&chunk);
	}
	Ok(buf.freeze())
}

async fn scenario(version: Version, ours_is_client: bool) {
	let label = format!(
		"{version:?}, ours is the {}",
		if ours_is_client { "client" } else { "server" }
	);
	let (ours, theirs) = ours_and_theirs(version, ours_is_client).await;
	assert_eq!(ours.protocol(), Some(PROTOCOL), "{label}");
	assert_eq!(theirs.protocol(), Some(PROTOCOL), "{label}");

	// Ours -> theirs, many times the default stream window.
	let mut send = ours.open_uni(SendOptions::default()).await.unwrap();
	let writer = tokio::spawn(async move {
		write_all(&mut send, payload(1)).await;
		send.finish().unwrap();
		send.closed().await
	});
	let mut recv = theirs.accept_uni().await.unwrap();
	assert_eq!(recv.read_all().await.unwrap(), payload(1), "{label}");
	writer.await.unwrap().unwrap();

	// Theirs -> ours.
	let mut send = theirs.open_uni().await.unwrap();
	let writer = tokio::spawn(async move {
		send.write_all(&payload(2)).await.unwrap();
		send.finish().unwrap();
	});
	let mut recv = ours.accept_uni().await.unwrap();
	assert_eq!(read_all(&mut recv).await.unwrap(), payload(2), "{label}");
	writer.await.unwrap();

	// A bidirectional echo, opened by each side.
	let (mut send, mut recv) = ours.open_bi(SendOptions::default()).await.unwrap();
	write_all(&mut send, Bytes::from_static(b"ping")).await;
	send.finish().unwrap();
	let (mut their_send, mut their_recv) = theirs.accept_bi().await.unwrap();
	assert_eq!(their_recv.read_all().await.unwrap(), "ping", "{label}");
	their_send.write_all(b"pong").await.unwrap();
	their_send.finish().unwrap();
	assert_eq!(read_all(&mut recv).await.unwrap(), "pong", "{label}");

	let (mut their_send, mut their_recv) = theirs.open_bi().await.unwrap();
	their_send.write_all(b"ping").await.unwrap();
	their_send.finish().unwrap();
	let (mut send, mut recv) = ours.accept_bi().await.unwrap();
	assert_eq!(read_all(&mut recv).await.unwrap(), "ping", "{label}");
	write_all(&mut send, Bytes::from_static(b"pong")).await;
	send.finish().unwrap();
	assert_eq!(their_recv.read_all().await.unwrap(), "pong", "{label}");

	// Reset codes, each way. The reset waits for the accept: 0.6 never surfaces a stream
	// whose first frame is RESET_STREAM.
	let mut send = ours.open_uni(SendOptions::default()).await.unwrap();
	write_all(&mut send, Bytes::from_static(b"partial")).await;
	let mut their_recv = theirs.accept_uni().await.unwrap();
	send.reset(7);
	let err = their_recv.read_all().await.unwrap_err();
	assert!(
		matches!(err, qmux06::Error::StreamReset(code) if code.into_inner() == 7),
		"{label}: {err:?}"
	);

	let mut their_send = theirs.open_uni().await.unwrap();
	their_send.write_all(b"partial").await.unwrap();
	let mut recv = ours.accept_uni().await.unwrap();
	their_send.reset(8);
	let err = read_all(&mut recv).await.unwrap_err();
	assert!(matches!(err, qmux::Error::StreamReset(8)), "{label}: {err:?}");

	// Datagrams, where records carry them.
	if version.uses_records() {
		ours.send_datagram(Bytes::from_static(b"ours"), SendOptions::default())
			.unwrap();
		assert_eq!(theirs.recv_datagram().await.unwrap(), "ours", "{label}");
		theirs.send_datagram(Bytes::from_static(b"theirs")).unwrap();
		assert_eq!(ours.recv_datagram().await.unwrap(), "theirs", "{label}");
	}

	// An application close, from each side in turn across the two roles.
	if ours_is_client {
		ours.close(42, "done");
		let err = theirs.closed().await;
		assert!(
			matches!(&err, qmux06::Error::ConnectionClosed { code, reason } if code.into_inner() == 42 && reason == "done"),
			"{label}: {err:?}"
		);
	} else {
		theirs.close(43, "done");
		let err = ours.closed().await;
		assert!(
			matches!(&err, qmux::Error::ConnectionClosed { code: 43, reason } if reason == "done"),
			"{label}: {err:?}"
		);
	}
}

#[tokio::test(start_paused = true)]
async fn ours_as_client() {
	for &version in Version::ALL {
		scenario(version, true).await;
	}
}

#[tokio::test(start_paused = true)]
async fn ours_as_server() {
	for &version in Version::ALL {
		scenario(version, false).await;
	}
}
